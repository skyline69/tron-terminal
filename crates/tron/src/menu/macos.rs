//! The AppKit menu bar.
//!
//! Menu items call `performMenuItem:` on a `TronMenuTarget`, which queues the
//! item's command and wakes the event loop; `App::proxy_wake_up` runs it.

use std::cell::RefCell;
use std::sync::{Mutex, OnceLock, PoisonError};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Imp, NSObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::NSString;
use tron_config::{Binding, KeyCombo};
use winit::event_loop::EventLoopProxy;

use super::{Item, MenuCommand};

/// Commands of chosen menu items, waiting for the event loop.
static COMMANDS: Mutex<Vec<MenuCommand>> = Mutex::new(Vec::new());
/// Wakes the event loop when a command is queued.
static PROXY: OnceLock<EventLoopProxy> = OnceLock::new();

thread_local! {
    /// The installed menu bar. AppKit objects are main thread only, as is this.
    static MENU_BAR: RefCell<Option<MenuBar>> = const { RefCell::new(None) };
}

struct MenuBar {
    /// Menu items do not retain their target, so the target lives here.
    _target: Retained<MenuTarget>,
    /// tron's own items, whose shortcuts follow the key bindings.
    items: Vec<(Item, Retained<NSMenuItem>)>,
    /// Shown when the Dock icon is right-clicked; AppKit adds the window list and Options itself.
    dock: Retained<NSMenu>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `MenuTarget` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TronMenuTarget"]
    struct MenuTarget;

    impl MenuTarget {
        // SAFETY: menu actions take the sending item and return nothing, matching this signature.
        #[unsafe(method(performMenuItem:))]
        fn perform(&self, sender: &NSMenuItem) {
            let Some(item) = Item::from_tag(sender.tag()) else { return };
            COMMANDS.lock().unwrap_or_else(PoisonError::into_inner).push(item.command());
            if let Some(proxy) = PROXY.get() {
                proxy.wake_up();
            }
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `init` is NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// Commands chosen from the menu since the last call.
pub fn take_commands() -> Vec<MenuCommand> {
    std::mem::take(&mut *COMMANDS.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Replaces winit's default menu bar with tron's. Must run on the main thread
/// once the event loop runs, since winit installs its menu when it starts.
pub fn install(proxy: EventLoopProxy, bindings: &[Binding]) {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("the menu bar can only be installed on the main thread");
        return;
    };
    if MENU_BAR.with_borrow(Option::is_some) {
        return;
    }
    let _ = PROXY.set(proxy);
    let app = NSApplication::sharedApplication(mtm);
    let mut builder = Builder { mtm, target: MenuTarget::new(mtm), items: Vec::new() };
    let bar = NSMenu::new(mtm);
    let command = NSEventModifierFlags::Command;
    let none = NSEventModifierFlags::empty();

    // The system shows the application name as this menu's title, whatever it is set to.
    let menu = builder.submenu(&bar, "tron");
    builder.standard(&menu, "About tron", sel!(orderFrontStandardAboutPanel:), "", none);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    builder.item(&menu, Item::Settings);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let services = builder.submenu(&menu, "Services");
    app.setServicesMenu(Some(&services));
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    builder.standard(&menu, "Hide tron", sel!(hide:), "h", command);
    builder.standard(&menu, "Hide Others", sel!(hideOtherApplications:), "h", command | NSEventModifierFlags::Option);
    builder.standard(&menu, "Show All", sel!(unhideAllApplications:), "", none);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    builder.standard(&menu, "Quit tron", sel!(terminate:), "q", command);

    let menu = builder.submenu(&bar, "File");
    builder.item(&menu, Item::NewWindow);
    // Reaches winit as `CloseRequested`.
    builder.standard(&menu, "Close Window", sel!(performClose:), "w", command);

    let menu = builder.submenu(&bar, "Edit");
    builder.items(&menu, &[Item::Copy, Item::Paste, Item::PasteSelection]);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    builder.item(&menu, Item::Find);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    builder.items(&menu, &[Item::SelectCommandOutput, Item::CopyCommandOutput, Item::ClearScrollback]);

    let menu = builder.submenu(&bar, "View");
    builder.items(&menu, &[Item::IncreaseFontSize, Item::DecreaseFontSize, Item::ResetFontSize]);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    builder.items(&menu, &[Item::PreviousPrompt, Item::NextPrompt, Item::ScrollToTop, Item::ScrollToBottom]);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    builder.item(&menu, Item::ReloadConfig);

    let menu = builder.submenu(&bar, "Window");
    builder.standard(&menu, "Minimize", sel!(performMiniaturize:), "m", command);
    builder.standard(&menu, "Zoom", sel!(performZoom:), "", none);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    builder.standard(&menu, "Bring All to Front", sel!(arrangeInFront:), "", none);
    // AppKit lists the open windows at the end of this menu.
    app.setWindowsMenu(Some(&menu));

    let menu = builder.submenu(&bar, "Help");
    builder.items(&menu, &[Item::Documentation, Item::ReportIssue]);
    // AppKit adds its search field to this menu.
    app.setHelpMenu(Some(&menu));

    app.setMainMenu(Some(&bar));

    let dock = NSMenu::new(mtm);
    builder.item(&dock, Item::NewWindow);
    MENU_BAR.set(Some(MenuBar { _target: builder.target, items: builder.items, dock }));
    add_dock_menu(&app);
    update(bindings);
}

/// Makes the application delegate answer `applicationDockMenu:` with `MenuBar::dock`.
///
/// AppKit asks the delegate for the Dock menu, and the delegate is winit's own
/// class, which tron cannot subclass. The method is added to that class at
/// runtime instead; `class_addMethod` leaves the class unchanged if winit ever
/// implements the method itself.
fn add_dock_menu(app: &NSApplication) {
    /// `- (NSMenu *)applicationDockMenu:(NSApplication *)sender`
    extern "C-unwind" fn dock_menu(_this: *mut AnyObject, _cmd: Sel, _sender: *mut AnyObject) -> *const NSMenu {
        // Not owned by the caller: `MENU_BAR` keeps the menu alive. AppKit asks on the main thread.
        MENU_BAR.with_borrow(|menu_bar| menu_bar.as_ref().map_or(std::ptr::null(), |bar| Retained::as_ptr(&bar.dock)))
    }
    type DockMenu = extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> *const NSMenu;

    let Some(delegate) = app.delegate() else {
        log::warn!("no application delegate for the Dock menu");
        return;
    };
    let object: &AnyObject = (*delegate).as_ref();
    let class = std::ptr::from_ref(object.class()).cast_mut();
    // SAFETY: the implementation takes self, the selector and one object and returns an
    // object, as the type encoding `@@:@` says, which is the signature AppKit calls
    // `applicationDockMenu:` with. Function pointers all have the size of `Imp`.
    let added = unsafe {
        let imp = std::mem::transmute::<DockMenu, Imp>(dock_menu);
        objc2::ffi::class_addMethod(class, sel!(applicationDockMenu:), imp, c"@@:@".as_ptr())
    };
    if !added.as_bool() {
        log::warn!("the application delegate already has a Dock menu");
        return;
    }
    // AppKit may check which optional methods a delegate implements when it is set,
    // so set it again now that it has one more.
    app.setDelegate(Some(&delegate));
}

/// Shows the shortcuts of `bindings` on the menu items, after the configuration
/// changed. Does nothing before the menu bar is installed.
pub fn update(bindings: &[Binding]) {
    MENU_BAR.with_borrow(|menu_bar| {
        let Some(menu_bar) = menu_bar else { return };
        for (item, menu_item) in &menu_bar.items {
            let (key, modifiers) = match item.shortcut(bindings) {
                Some((key, combo)) => (NSString::from_str(&key), modifier_flags(combo)),
                None => (NSString::from_str(""), NSEventModifierFlags::empty()),
            };
            menu_item.setKeyEquivalent(&key);
            menu_item.setKeyEquivalentModifierMask(modifiers);
        }
    });
}

fn modifier_flags(combo: &KeyCombo) -> NSEventModifierFlags {
    let mut flags = NSEventModifierFlags::empty();
    for (held, flag) in [
        (combo.super_key, NSEventModifierFlags::Command),
        (combo.shift, NSEventModifierFlags::Shift),
        (combo.alt, NSEventModifierFlags::Option),
        (combo.ctrl, NSEventModifierFlags::Control),
    ] {
        if held {
            flags |= flag;
        }
    }
    flags
}

struct Builder {
    mtm: MainThreadMarker,
    target: Retained<MenuTarget>,
    items: Vec<(Item, Retained<NSMenuItem>)>,
}

impl Builder {
    /// Adds a submenu titled `title` to `parent`.
    fn submenu(&self, parent: &NSMenu, title: &str) -> Retained<NSMenu> {
        let title = NSString::from_str(title);
        let menu = NSMenu::initWithTitle(self.mtm.alloc(), &title);
        let item = NSMenuItem::new(self.mtm);
        item.setTitle(&title);
        item.setSubmenu(Some(&menu));
        parent.addItem(&item);
        menu
    }

    /// Adds an item sending `action` through the responder chain, to the key window or the application.
    fn standard(&self, menu: &NSMenu, title: &str, action: Sel, key: &str, modifiers: NSEventModifierFlags) {
        // SAFETY: `action` is one of AppKit's standard action selectors, taking the sender.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                self.mtm.alloc(),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            )
        };
        item.setKeyEquivalentModifierMask(modifiers);
        menu.addItem(&item);
    }

    /// Adds one of tron's items. Its shortcut is set later, by `update`.
    fn item(&mut self, menu: &NSMenu, item: Item) {
        // SAFETY: `performMenuItem:` is defined on `MenuTarget` and takes the sender.
        let menu_item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                self.mtm.alloc(),
                &NSString::from_str(item.title()),
                Some(sel!(performMenuItem:)),
                &NSString::from_str(""),
            )
        };
        menu_item.setTag(item.tag());
        // SAFETY: the target is a `MenuTarget`, which implements the item's action, and
        // `MenuBar` keeps it alive for as long as the menu exists.
        unsafe { menu_item.setTarget(Some(&self.target)) };
        menu.addItem(&menu_item);
        self.items.push((item, menu_item));
    }

    fn items(&mut self, menu: &NSMenu, items: &[Item]) {
        for &item in items {
            self.item(menu, item);
        }
    }
}
