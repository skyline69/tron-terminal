//! macOS integration: Option as Alt, notifications, the title bar folder icon and Look Up.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::rc::{Rc, Weak};

use objc2::runtime::{AnyObject, Imp, Sel};
use objc2::{AllocAnyThread, MainThreadMarker, sel};
use objc2_app_kit::{
    NSEvent, NSFont, NSFontAttributeName, NSFontManager, NSFontTraitMask, NSFontWeightRegular, NSView,
};
use objc2_foundation::{NSAttributedString, NSDictionary, NSPoint, NSString, NSURL};
use tron_config::OptionAsAlt as Setting;
use winit::event::Modifiers;
use winit::keyboard::ModifiersKeyState;
use winit::platform::macos::{OptionAsAlt, WindowAttributesMacOS, WindowExtMacOS};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowAttributes};

/// The window's content view.
fn content_view(window: &dyn Window) -> Option<&NSView> {
    let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else { return None };
    // SAFETY: winit's handle is the window's content view, used on the main thread
    // while the window is alive.
    Some(unsafe { handle.ns_view.cast().as_ref() })
}

/// A word to show in the Look Up panel.
pub struct LookUpWord {
    pub text: String,
    /// Where the word's text baseline starts, in points from the view's top left.
    pub baseline: (f64, f64),
    pub font_family: String,
    pub font_size: f64,
}

/// Finds the word under a point of a window's view, in points from its top left.
pub trait WordAt {
    fn word_at(&self, x: f64, y: f64) -> Option<LookUpWord>;
}

thread_local! {
    /// Each window's word finder, by its view. A closed window's entry stops answering.
    static LOOK_UP: RefCell<HashMap<usize, Weak<dyn WordAt>>> = RefCell::new(HashMap::new());
}

/// Shows the dictionary panel for the word `target` finds when a word in the window
/// is force clicked or tapped with three fingers.
///
/// AppKit sends `quickLookWithEvent:` to the view, which is winit's own class. The
/// method is added to that class at runtime; `class_addMethod` leaves the class
/// unchanged when it is already there.
pub fn install_look_up(window: &dyn Window, target: &Rc<dyn WordAt>) {
    type QuickLook = extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject);
    let Some(view) = content_view(window) else { return };
    let object: &AnyObject = view.as_ref();
    let class = std::ptr::from_ref(object.class()).cast_mut();
    // SAFETY: `quick_look` takes self, the selector and one object and returns nothing,
    // as the type encoding `v@:@` says, which is how AppKit calls `quickLookWithEvent:`.
    // Function pointers all have the size of `Imp`.
    unsafe {
        let imp = std::mem::transmute::<QuickLook, Imp>(quick_look);
        objc2::ffi::class_addMethod(class, sel!(quickLookWithEvent:), imp, c"v@:@".as_ptr());
    }
    LOOK_UP.with_borrow_mut(|targets| {
        targets.retain(|_, target| target.strong_count() > 0);
        targets.insert(std::ptr::from_ref(view).addr(), Rc::downgrade(target));
    });
}

/// `- (void)quickLookWithEvent:(NSEvent *)event`
extern "C-unwind" fn quick_look(this: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    // SAFETY: AppKit calls this on the main thread with the view and a live event.
    let (view, event) = unsafe { (&*this.cast::<NSView>(), &*event.cast::<NSEvent>()) };
    // winit's view is flipped: the point is measured from its top left.
    let point = view.convertPoint_fromView(event.locationInWindow(), None);
    let key = std::ptr::from_ref(view).addr();
    let target = LOOK_UP.with_borrow(|targets| targets.get(&key).and_then(Weak::upgrade));
    let Some(word) = target.and_then(|target| target.word_at(point.x, point.y)) else { return };

    // The panel draws the word over the terminal's text, so it uses the terminal's font.
    let family = NSString::from_str(&word.font_family);
    let size = word.font_size;
    let font = NSFont::fontWithName_size(&family, size)
        .or_else(|| {
            NSFontManager::sharedFontManager(mtm).fontWithFamily_traits_weight_size(
                &family,
                NSFontTraitMask::empty(),
                5,
                size,
            )
        })
        // SAFETY: AppKit's constant, read-only.
        .unwrap_or_else(|| NSFont::monospacedSystemFontOfSize_weight(size, unsafe { NSFontWeightRegular }));
    let font: &AnyObject = &font;
    // SAFETY: AppKit's constant, read-only.
    let attributes = NSDictionary::from_slices(&[unsafe { NSFontAttributeName }], &[font]);
    // SAFETY: the font attribute's value is an NSFont.
    let text = unsafe {
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &NSString::from_str(&word.text),
            Some(&attributes),
        )
    };
    view.showDefinitionForAttributedString_atPoint(Some(&text), NSPoint::new(word.baseline.0, word.baseline.1));
}

/// Shows `directory` as the window's proxy icon, the folder icon beside the
/// title. It can be dragged, and Command-clicking it lists the parent folders.
pub fn set_represented_directory(window: &dyn Window, directory: Option<&Path>) {
    let Some(view) = content_view(window) else { return };
    let Some(ns_window) = view.window() else { return };
    let url = directory
        .and_then(Path::to_str)
        .map(|path| NSURL::fileURLWithPath_isDirectory(&NSString::from_str(path), true));
    ns_window.setRepresentedURL(url.as_deref());
}

fn option_as_alt(setting: Setting) -> OptionAsAlt {
    match setting {
        Setting::None => OptionAsAlt::None,
        Setting::Left => OptionAsAlt::OnlyLeft,
        Setting::Right => OptionAsAlt::OnlyRight,
        Setting::Both => OptionAsAlt::Both,
    }
}

pub fn window_attributes(attributes: WindowAttributes, setting: Setting) -> WindowAttributes {
    // The terminal background, with its opacity and shaders, extends under a
    // transparent title bar. A translucent window otherwise leaves the title bar
    // without any background. Text starts below it, see `top_inset` in main.rs.
    let platform = WindowAttributesMacOS::default()
        .with_option_as_alt(option_as_alt(setting))
        .with_titlebar_transparent(true)
        .with_fullsize_content_view(true);
    attributes.with_platform_attributes(Box::new(platform))
}

pub fn set_option_as_alt(window: &dyn Window, setting: Setting) {
    let value = option_as_alt(setting);
    if window.option_as_alt() != value {
        window.set_option_as_alt(value);
    }
}

/// Whether the left and right Option keys are held.
pub fn option_keys(modifiers: &Modifiers) -> (bool, bool) {
    (modifiers.lalt_state() == ModifiersKeyState::Pressed, modifiers.ralt_state() == ModifiersKeyState::Pressed)
}

/// Whether the held Option keys act as Alt rather than composing characters.
pub fn option_is_alt(setting: Setting, (left, right): (bool, bool)) -> bool {
    match setting {
        Setting::None => false,
        Setting::Left => left,
        Setting::Right => right,
        Setting::Both => true,
    }
}

/// `program --app-name tron -- title body`, or a Notification Center banner when
/// `program` is `osascript`. The text is passed as arguments, never as script source.
pub fn notification(program: &str, title: &str, body: &str) -> Command {
    let mut command = Command::new(program);
    if program == "osascript" {
        command.args([
            "-e",
            "on run argv",
            "-e",
            "display notification (item 2 of argv) with title (item 1 of argv)",
            "-e",
            "end run",
            title,
            body,
        ]);
    } else {
        command.args(["--app-name", "tron", "--", title, body]);
    }
    command
}
