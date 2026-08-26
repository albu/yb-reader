local _ = require("gettext")
return {
    fullname = _("Screen Mirror"),
    description = _([[
Mirror a window from a Mac over WiFi.

Left third of the screen taps = previous page, elsewhere = next page
(the server turns taps into real ←/→ arrow keys). Swipe down to quit.

"Fetch book from Mac" pulls the single file the Mac's send.py is holding
into /documents; the Mac-side server exits by itself once received.

Configure the Mac address in /mnt/us/extensions/mirror/mirror.conf]]),
}
