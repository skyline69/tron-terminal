# dmgbuild settings for tron's macOS disk image. The release workflow runs:
#
#   dmgbuild -s dist/macos/dmg-settings.py \
#       -D app=path/to/tron.app -D background=background.tiff -D icon=tron.icns \
#       "tron" tron.dmg
#
# The window shows tron.app and an Applications link on the background from
# background.svg, which draws an arrow between them.

import os.path

app = defines["app"]  # noqa: F821, dmgbuild provides `defines`
app_name = os.path.basename(app)

format = "UDZO"
filesystem = "HFS+"
files = [app]
symlinks = {"Applications": "/Applications"}
hide_extensions = [app_name]
icon = defines.get("icon")  # noqa: F821

# The window matches the 660x400 point background.
background = defines["background"]  # noqa: F821
window_rect = ((200, 120), (660, 400))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False

# Icon centers, in points from the top left, over the background's shelf.
icon_size = 112
text_size = 13
icon_locations = {
    app_name: (170, 186),
    "Applications": (490, 186),
}
arrange_by = None
