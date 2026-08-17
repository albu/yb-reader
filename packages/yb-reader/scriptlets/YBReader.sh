#!/bin/sh
# Name: YB Reader
# Author: yb
# Icon: data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAAAAACPAi4CAAAAYElEQVR42u3X0QoAEAyFYU/iwbz/e8ytsCybUP+5Xl/R0pHEmQQQBuSNAGjAkpLDwPow7ewIiAUQFTBdZzfOIgHMgGIJAAAAgAXgUf0fCCgYzooTULIe6IlsogPg33gVqJUfZXbeCS+YAAAAAElFTkSuQmCC
# DontUseFBInk

# Started like a book from the Kindle library, exactly like KOReader.sh.
# # DontUseFBInk: the framework normally pipes stdout/stderr to FBInk, which
# would fight our own framebuffer drawing.
cd /mnt/us/extensions/reader/bin
exec ./start.sh
