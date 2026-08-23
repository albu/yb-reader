//! Receive over Wi-Fi — the Kindle becomes the server. A hand-rolled HTTP
//! listener (same "one static binary, no heavy deps" rule as protocol.rs's
//! client) serves a drag-drop page and streams raw POST bodies straight to
//! disk — never buffered in RAM. The screen shows a QR of the URL; any
//! phone/laptop browser on the LAN is then the client, which deletes the
//! whole "Kindle must find the Mac" discovery dance fetch.rs carries.
//!
//! Delivery semantics match fetch.rs: a file counts as received only after
//! the full body is written, fsynced and renamed into documents/.

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::os::unix::io::FromRawFd;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use qrcode::{Color, QrCode};
use ybdev::config::{sanitize_fetch_name, urldecode};
use ybdev::input::Gesture;
use ybdev::log::{now_ms, plog};

use crate::wifi;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

/// Overridable so the host-side test can aim the handler at a temp dir.
fn save_dir() -> String {
    std::env::var("YB_SAVE_DIR").unwrap_or_else(|_| "/mnt/us/documents".to_string())
}

/// Same allowlist as library::list_books: anything else would land in
/// documents/ but never appear in the library. (mobi/azw3 are refused —
/// there is no parser for them; converting to epub is the supported path.)
const OK_EXTS: [&str; 5] = ["epub", "pdf", "fb2", "txt", "cbz"];
/// The screensaver root accepts exactly what the sleep screen renders.
const SS_EXTS: [&str; 3] = ["png", "jpg", "jpeg"];

/// The two browsable roots, strictly enumerated — a root is never a
/// path, so a hostile ?root= cannot escape into the filesystem. The
/// screensavers dir is env-overridable for the host-side test, like
/// save_dir().
fn base_dir(root: Option<&str>) -> Option<String> {
    match root.unwrap_or("documents") {
        "documents" => Some(save_dir()),
        "screensavers" => {
            Some(std::env::var("YB_SS_DIR").unwrap_or_else(|_| "/mnt/us/screensavers".to_string()))
        }
        _ => None,
    }
}

fn root_param(query: &str) -> Option<String> {
    query_param(query, "root")
}
const MAX_BODY: u64 = 512 * 1024 * 1024;
/// Wall-clock caps bounding a whole transaction, not just one read(). The
/// per-read 60s socket timeout resets on every byte, so a client dripping
/// 1 B/59 s could otherwise pin the single-threaded accept loop forever.
/// Headers are tiny: 30 s. Bodies ride a generous half hour — enough for
/// MAX_BODY over slow Wi-Fi — after which the connection is dropped.
const HEADER_PHASE_CAP: Duration = Duration::from_secs(30);
const BODY_PHASE_CAP: Duration = Duration::from_secs(30 * 60);
const HDR_CAP: usize = 16 * 1024;
const PORT: u16 = 8080;
const IPTABLES: &str = "/usr/sbin/iptables";

const SYSTEM_FILES: [&str; 2] = ["My Clippings.txt", "JAILBROKEN.txt"];

/// The web file manager: responsive mobile & desktop UI for browsing,
/// uploading, previewing, moving, creating folders, and deleting books.
const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no">
<title>yb-reader &mdash; Kindle File Manager</title>
<style>
:root {
  --bg: #0d0e12;
  --surface: #16181f;
  --surface-hover: #1e212b;
  --surface-active: #262a36;
  --border: rgba(255, 255, 255, 0.08);
  --border-subtle: rgba(255, 255, 255, 0.04);
  --text-main: #f3f4f6;
  --text-muted: #9ca3af;
  --text-dim: #6b7280;
  --accent: #3b82f6;
  --accent-glow: rgba(59, 130, 246, 0.18);
  --accent-text: #60a5fa;
  --danger: #ef4444;
  --danger-bg: rgba(239, 68, 68, 0.12);
  --success: #10b981;
  --radius-lg: 14px;
  --radius-md: 10px;
  --radius-sm: 6px;
}

@media (prefers-color-scheme: light) {
  :root {
    --bg: #f8fafc;
    --surface: #ffffff;
    --surface-hover: #f1f5f9;
    --surface-active: #e2e8f0;
    --border: rgba(0, 0, 0, 0.08);
    --border-subtle: rgba(0, 0, 0, 0.04);
    --text-main: #0f172a;
    --text-muted: #64748b;
    --text-dim: #94a3b8;
    --accent: #2563eb;
    --accent-glow: rgba(37, 99, 235, 0.12);
    --accent-text: #2563eb;
    --danger: #dc2626;
    --danger-bg: rgba(220, 38, 38, 0.08);
    --success: #059669;
  }
}

* { box-sizing: border-box; margin: 0; padding: 0; font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Display', 'Inter', 'Segoe UI', Roboto, sans-serif; }
body { background: var(--bg); color: var(--text-main); min-height: 100vh; -webkit-font-smoothing: antialiased; padding: 24px 20px; }

.app-container { max-width: 1040px; margin: 0 auto; }

/* Top Navigation Bar */
.navbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 18px;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  margin-bottom: 20px;
  backdrop-filter: blur(12px);
  gap: 12px;
}

.brand { display: flex; align-items: center; gap: 12px; }
.brand-title { font-size: 1.05rem; font-weight: 700; letter-spacing: -0.3px; color: var(--text-main); }
.device-tag {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 0.75rem;
  font-weight: 600;
  color: var(--text-muted);
  background: var(--border-subtle);
  border: 1px solid var(--border);
  padding: 3px 9px;
  border-radius: 20px;
}
.status-dot { width: 6px; height: 6px; background: var(--success); border-radius: 50%; box-shadow: 0 0 8px var(--success); }

/* Center Pill Nav */
.nav-switcher {
  display: flex;
  background: var(--bg);
  padding: 3px;
  border-radius: var(--radius-md);
  border: 1px solid var(--border);
}
.nav-tab {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 7px 18px;
  border-radius: 8px;
  font-size: 0.85rem;
  font-weight: 600;
  color: var(--text-muted);
  background: transparent;
  border: none;
  cursor: pointer;
  transition: all 0.15s ease;
}
.nav-tab:hover { color: var(--text-main); }
.nav-tab.active { background: var(--surface); color: var(--text-main); box-shadow: 0 2px 8px rgba(0,0,0,0.1); }

/* Nav Stats */
.nav-stats { display: flex; align-items: center; gap: 12px; }
.storage-indicator {
  font-size: 0.8rem;
  font-weight: 600;
  color: var(--text-muted);
  display: flex;
  align-items: center;
  gap: 6px;
  white-space: nowrap;
}

/* Hero Upload Dropzone */
.dropzone {
  border: 1.5px dashed var(--border);
  background: var(--surface);
  border-radius: var(--radius-lg);
  padding: 32px 24px;
  text-align: center;
  cursor: pointer;
  transition: all 0.2s ease;
  margin-bottom: 24px;
  position: relative;
}
.dropzone:hover, .dropzone.dragover { border-color: var(--accent); background: var(--accent-glow); transform: translateY(-1px); }
.drop-icon-wrap {
  width: 44px;
  height: 44px;
  border-radius: 12px;
  background: var(--accent-glow);
  color: var(--accent-text);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  margin-bottom: 10px;
}
.drop-title { font-size: 0.95rem; font-weight: 600; color: var(--text-main); margin-bottom: 4px; }
.drop-title span { color: var(--accent-text); text-decoration: underline; text-underline-offset: 3px; }
.drop-subtitle { font-size: 0.78rem; color: var(--text-dim); }

/* Action & Search Bar */
.action-bar { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 16px; flex-wrap: wrap; }
.search-input-wrap { position: relative; flex: 1; min-width: 220px; }
.search-input-wrap svg { position: absolute; left: 12px; top: 50%; transform: translateY(-50%); color: var(--text-dim); }
.search-input { width: 100%; padding: 9px 12px 9px 36px; font-size: 0.88rem; border-radius: var(--radius-md); border: 1px solid var(--border); background: var(--surface); color: var(--text-main); outline: none; transition: border-color 0.15s; }
.search-input:focus { border-color: var(--accent); }

.action-buttons { display: flex; align-items: center; gap: 8px; }
.btn { display: inline-flex; align-items: center; gap: 6px; padding: 8px 14px; border-radius: var(--radius-md); font-size: 0.85rem; font-weight: 600; border: 1px solid var(--border); background: var(--surface); color: var(--text-main); cursor: pointer; transition: all 0.15s; }
.btn:hover { background: var(--surface-hover); border-color: rgba(255,255,255,0.15); }
.btn-primary { background: var(--accent); border-color: var(--accent); color: #fff; }
.btn-primary:hover { background: #2563eb; }

/* Breadcrumbs bar */
.breadcrumbs-bar {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 8px 14px;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  margin-bottom: 14px;
  font-size: 0.85rem;
}
.crumb { color: var(--text-muted); text-decoration: none; cursor: pointer; display: inline-flex; align-items: center; gap: 4px; }
.crumb:hover { color: var(--accent-text); }
.crumb.current { color: var(--text-main); font-weight: 600; cursor: default; }

.section-label {
  font-size: 0.75rem;
  font-weight: 700;
  letter-spacing: 0.5px;
  text-transform: uppercase;
  color: var(--text-dim);
  margin: 18px 0 8px 4px;
}

/* Folders Grid in Books */
.folders-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
  gap: 10px;
  margin-bottom: 18px;
}
.folder-pill {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 14px;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  cursor: pointer;
  transition: all 0.15s;
  text-decoration: none;
  color: inherit;
}
.folder-pill:hover {
  background: var(--surface-hover);
  border-color: rgba(255,255,255,0.18);
  transform: translateY(-1px);
}
.folder-icon { color: #f59e0b; flex-shrink: 0; }
.folder-name { font-size: 0.88rem; font-weight: 600; color: var(--text-main); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }

/* Books Table View */
.books-table {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  overflow: hidden;
}
.book-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 18px;
  border-bottom: 1px solid var(--border-subtle);
  transition: background 0.15s;
  gap: 12px;
}
.book-row:last-child { border-bottom: none; }
.book-row:hover { background: var(--surface-hover); }

.book-left {
  display: flex;
  align-items: center;
  gap: 14px;
  min-width: 0;
  flex: 1;
}
.format-badge {
  font-size: 0.7rem;
  font-weight: 700;
  letter-spacing: 0.5px;
  padding: 4px 8px;
  border-radius: 6px;
  background: var(--surface-active);
  border: 1px solid var(--border);
  color: var(--accent-text);
  width: 48px;
  text-align: center;
  flex-shrink: 0;
}
.format-badge.pdf { color: #ef4444; }
.format-badge.cbz { color: #a855f7; }

.book-meta-box { min-width: 0; flex: 1; }
.book-title { font-size: 0.92rem; font-weight: 600; color: var(--text-main); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.book-size { font-size: 0.78rem; color: var(--text-dim); margin-top: 2px; }

.book-actions { display: flex; align-items: center; gap: 6px; flex-shrink: 0; }
.icon-btn {
  width: 32px;
  height: 32px;
  border-radius: 8px;
  border: 1px solid var(--border);
  background: var(--surface);
  color: var(--text-muted);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: all 0.15s;
  text-decoration: none;
}
.icon-btn:hover { color: var(--text-main); background: var(--surface-active); border-color: rgba(255,255,255,0.18); }
.icon-btn.btn-delete:hover { color: var(--danger); background: var(--danger-bg); border-color: var(--danger); }

/* Grid View for Screensavers */
.screensavers-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
  gap: 16px;
}

.poster-card {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  overflow: hidden;
  position: relative;
  transition: all 0.2s ease;
  display: flex;
  flex-direction: column;
}
.poster-card:hover {
  transform: translateY(-3px);
  border-color: rgba(255,255,255,0.2);
  box-shadow: 0 12px 24px rgba(0,0,0,0.25);
}
.poster-img-wrap {
  position: relative;
  width: 100%;
  aspect-ratio: 3 / 4;
  background: #000;
  overflow: hidden;
  cursor: pointer;
}
.poster-img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  transition: transform 0.3s ease;
}
.poster-card:hover .poster-img {
  transform: scale(1.03);
}
.poster-overlay {
  position: absolute;
  inset: 0;
  background: linear-gradient(180deg, transparent 60%, rgba(0,0,0,0.8) 100%);
  opacity: 0.9;
}
.poster-body {
  padding: 12px 14px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
.poster-info { min-width: 0; flex: 1; }
.poster-name {
  font-size: 0.88rem;
  font-weight: 600;
  color: var(--text-main);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.poster-meta { font-size: 0.75rem; color: var(--text-dim); margin-top: 2px; }

/* Empty state */
.empty-box {
  text-align: center;
  padding: 48px 20px;
  color: var(--text-dim);
  font-size: 0.9rem;
  background: var(--surface);
  border-radius: var(--radius-lg);
  border: 1px solid var(--border);
}

/* Floating Upload HUD */
.upload-hud {
  position: fixed;
  bottom: 24px;
  right: 24px;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  padding: 14px 18px;
  box-shadow: 0 16px 36px rgba(0,0,0,0.3);
  display: none;
  align-items: center;
  gap: 14px;
  z-index: 100;
  backdrop-filter: blur(12px);
  min-width: 280px;
}
.hud-progress { flex: 1; }
.hud-title {
  font-size: 0.82rem;
  font-weight: 600;
  color: var(--text-main);
  margin-bottom: 6px;
  display: flex;
  justify-content: space-between;
}
.hud-bar {
  width: 100%;
  height: 6px;
  background: var(--surface-active);
  border-radius: 3px;
  overflow: hidden;
}
.hud-bar-fill {
  height: 100%;
  background: var(--accent);
  width: 0%;
  border-radius: 3px;
  transition: width 0.15s ease-out;
}

/* Modals */
.modal-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0,0,0,0.65);
  backdrop-filter: blur(6px);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 999;
  padding: 16px;
  opacity: 0;
  pointer-events: none;
  transition: opacity 0.2s;
}
.modal-overlay.active { opacity: 1; pointer-events: auto; }
.modal {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  padding: 24px;
  width: 100%;
  max-width: 440px;
  box-shadow: 0 20px 40px rgba(0,0,0,0.4);
  transform: scale(0.96);
  transition: transform 0.2s;
}
.modal-overlay.active .modal { transform: scale(1); }
.modal h2 { font-size: 1.15rem; margin-bottom: 12px; font-weight: 700; color: var(--text-main); }
.modal p { font-size: 0.88rem; color: var(--text-muted); margin-bottom: 16px; }
.modal input, .modal select {
  width: 100%;
  padding: 10px 12px;
  font-size: 0.92rem;
  border-radius: var(--radius-md);
  border: 1px solid var(--border);
  background: var(--bg);
  color: var(--text-main);
  margin-bottom: 18px;
  outline: none;
}
.modal input:focus, .modal select:focus { border-color: var(--accent); }
.modal-actions { display: flex; justify-content: flex-end; gap: 10px; }

.preview-modal { max-width: 620px; text-align: center; padding: 18px; }
.preview-modal img {
  max-width: 100%;
  max-height: 70vh;
  object-fit: contain;
  border-radius: var(--radius-md);
  border: 1px solid var(--border);
  margin-bottom: 14px;
  background: #000;
}

/* Toast */
.toast {
  position: fixed;
  bottom: 24px;
  left: 50%;
  transform: translateX(-50%) translateY(20px);
  background: var(--surface);
  border: 1px solid var(--border);
  color: var(--text-main);
  padding: 10px 18px;
  border-radius: 20px;
  font-size: 0.85rem;
  font-weight: 500;
  opacity: 0;
  pointer-events: none;
  transition: all 0.25s ease-out;
  z-index: 1000;
  box-shadow: 0 10px 25px rgba(0,0,0,0.3);
}
.toast.active { opacity: 1; transform: translateX(-50%) translateY(0); }
.toast.success { border-color: var(--success); color: var(--success); }
.toast.error { border-color: var(--danger); color: var(--danger); }

.drag-overlay {
  position: fixed;
  inset: 0;
  background: var(--accent-glow);
  backdrop-filter: blur(4px);
  border: 3px dashed var(--accent);
  z-index: 998;
  display: none;
  align-items: center;
  justify-content: center;
  pointer-events: none;
  font-size: 1.2rem;
  font-weight: 700;
  color: var(--accent-text);
}
.drag-overlay.active { display: flex; }

@media (max-width: 768px) {
  body { padding: 14px 12px; }
  .navbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 12px 14px;
  }
  .brand { order: 1; }
  .nav-stats { order: 2; }
  .storage-indicator { font-size: 0.78rem; padding: 4px 8px; gap: 5px; }
  .nav-switcher {
    order: 3;
    width: 100%;
    display: grid;
    grid-template-columns: 1fr 1fr;
    box-sizing: border-box;
    gap: 4px;
  }
  .nav-tab {
    width: 100%;
    justify-content: center;
    padding: 7px 4px;
    font-size: 0.8rem;
    gap: 5px;
    box-sizing: border-box;
  }
  .screensavers-grid { grid-template-columns: repeat(2, 1fr); gap: 10px; }
  .dropzone { padding: 22px 14px; }
  .poster-body { padding: 10px 10px; }
}
</style>
</head>
<body>
<div class="app-container">

  <!-- Navigation Bar -->
  <header class="navbar">
    <div class="brand">
      <div class="brand-title">yb-reader</div>
      <div class="device-tag">
        <div class="status-dot"></div>
        <span>Kindle Paperwhite</span>
      </div>
    </div>

    <nav class="nav-switcher">
      <button class="nav-tab active" id="tab-books" onclick="setRoot('documents')">
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 19.5v-15A2.5 2.5 0 0 1 6.5 2H20v20H6.5a2.5 2.5 0 0 1-2.5-2.5Z"/><path d="M6 6h10"/><path d="M6 10h10"/></svg>
        Books
      </button>
      <button class="nav-tab" id="tab-screensavers" onclick="setRoot('screensavers')">
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect width="18" height="18" x="3" y="3" rx="2" ry="2"/><circle cx="9" cy="9" r="2"/><path d="m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21"/></svg>
        Screensavers
      </button>
    </nav>

    <div class="nav-stats">
      <div class="storage-indicator" id="storage-badge">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect width="20" height="8" x="2" y="2" rx="2" ry="2"/><rect width="20" height="8" x="2" y="14" rx="2" ry="2"/><line x1="6" x2="6.01" y1="6" y2="6"/><line x1="6" x2="6.01" y1="18" y2="18"/></svg>
        <span id="storage-text">Kindle Storage</span>
      </div>
    </div>
  </header>

  <!-- Hero Dropzone -->
  <div class="dropzone" id="dropzone" onclick="f.click()">
    <div class="drop-icon-wrap">
      <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="17 8 12 3 7 8"/><line x1="12" x2="12" y1="3" y2="15"/></svg>
    </div>
    <div class="drop-title" id="drop-title">Drag & drop books here, or <span>browse</span></div>
    <div class="drop-subtitle" id="drop-subtitle">Supports EPUB, PDF, CBZ, FB2, TXT</div>
  </div>

  <!-- Action Bar -->
  <div class="action-bar">
    <div class="search-input-wrap">
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/></svg>
      <input type="text" id="search-input" class="search-input" placeholder="Filter books..." oninput="filterCards()">
    </div>
    <div class="action-buttons">
      <button class="btn" id="new-folder-btn" onclick="openMkdir()">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.93a2 2 0 0 1-1.66-.9l-.82-1.2A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13c0 1.1.9 2 2 2Z"/><line x1="12" y1="10" x2="12" y2="16"/><line x1="9" y1="13" x2="15" y2="13"/></svg>
        New Folder
      </button>
    </div>
  </div>

  <!-- Breadcrumbs for Books -->
  <div class="breadcrumbs-bar" id="breadcrumbs" style="display:none;"></div>

  <!-- Content Container -->
  <div id="content-container"></div>

</div>

<!-- Upload Progress HUD -->
<div class="upload-hud" id="upload-hud">
  <div class="hud-progress">
    <div class="hud-title">
      <span id="hud-title">Uploading...</span>
      <span id="hud-pct">0%</span>
    </div>
    <div class="hud-bar"><div class="hud-bar-fill" id="hud-fill"></div></div>
  </div>
</div>

<!-- Modal: New Folder -->
<div class="modal-overlay" id="mkdir-modal">
  <div class="modal">
    <h2>New Folder</h2>
    <input type="text" id="mkdir-name" placeholder="Folder name (e.g. Sci-Fi, Technical)">
    <div class="modal-actions">
      <button class="btn" onclick="closeModal('mkdir-modal')">Cancel</button>
      <button class="btn btn-primary" onclick="submitMkdir()">Create</button>
    </div>
  </div>
</div>

<!-- Modal: Move File -->
<div class="modal-overlay" id="move-modal">
  <div class="modal">
    <h2>Move Item</h2>
    <p id="move-prompt"></p>
    <select id="move-target"></select>
    <div class="modal-actions">
      <button class="btn" onclick="closeModal('move-modal')">Cancel</button>
      <button class="btn btn-primary" onclick="submitMove()">Move</button>
    </div>
  </div>
</div>

<!-- Modal: Delete Confirmation -->
<div class="modal-overlay" id="delete-modal">
  <div class="modal">
    <h2>Delete Item?</h2>
    <p id="delete-prompt"></p>
    <div class="modal-actions">
      <button class="btn" onclick="closeModal('delete-modal')">Cancel</button>
      <button class="btn" style="background:var(--danger);color:#fff;border-color:var(--danger)" onclick="submitDelete()">Delete</button>
    </div>
  </div>
</div>

<!-- Modal: Image Preview -->
<div class="modal-overlay" id="preview-modal" onclick="closeModal('preview-modal')">
  <div class="modal preview-modal" onclick="event.stopPropagation()">
    <img id="preview-img" src="" alt="Preview">
    <div style="font-weight:600;font-size:0.92rem;margin-bottom:8px" id="preview-title"></div>
    <div class="modal-actions" style="justify-content:center">
      <a class="btn" id="preview-dl" download>Download Image</a>
      <button class="btn btn-primary" onclick="closeModal('preview-modal')">Close</button>
    </div>
  </div>
</div>

<div class="toast" id="toast"></div>
<div class="drag-overlay" id="drag-overlay">Drop files anywhere to upload</div>

<input type="file" id="f" multiple style="display:none">

<script>
let curDir = '';
let root = 'documents';
let allFolders = [];
let itemToMove = null;
let itemToDelete = null;

const dz = document.getElementById('dropzone');
const f = document.getElementById('f');
const contentCont = document.getElementById('content-container');
const breadcrumbs = document.getElementById('breadcrumbs');
const searchInput = document.getElementById('search-input');
const storageText = document.getElementById('storage-text');
const newFolderBtn = document.getElementById('new-folder-btn');
const dropTitle = document.getElementById('drop-title');
const dropSubtitle = document.getElementById('drop-subtitle');
const hud = document.getElementById('upload-hud');
const hudTitle = document.getElementById('hud-title');
const hudPct = document.getElementById('hud-pct');
const hudFill = document.getElementById('hud-fill');
const toastEl = document.getElementById('toast');
const dragOverlay = document.getElementById('drag-overlay');

function showToast(msg, type = '') {
  toastEl.textContent = msg;
  toastEl.className = 'toast active ' + type;
  setTimeout(() => { toastEl.className = 'toast'; }, 3000);
}

function setRoot(r) {
  root = r;
  curDir = '';
  document.getElementById('tab-books').classList.toggle('active', r === 'documents');
  document.getElementById('tab-screensavers').classList.toggle('active', r === 'screensavers');
  
  if (r === 'screensavers') {
    newFolderBtn.style.display = 'none';
    breadcrumbs.style.display = 'none';
    searchInput.placeholder = 'Filter screensavers...';
    dropTitle.innerHTML = 'Drag & drop screensavers here, or <span>browse</span>';
    dropSubtitle.textContent = 'Supports JPG, PNG · Optimized for 1236 × 1648 Paperwhite display';
    f.accept = '.png,.jpg,.jpeg';
  } else {
    newFolderBtn.style.display = 'inline-flex';
    searchInput.placeholder = 'Filter books...';
    dropTitle.innerHTML = 'Drag & drop books here, or <span>browse</span>';
    dropSubtitle.textContent = 'Supports EPUB, PDF, CBZ, FB2, TXT';
    f.accept = '.epub,.pdf,.cbz,.fb2,.txt';
  }
  searchInput.value = '';
  load(curDir);
}

function formatBytes(bytes) {
  if (bytes === 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i];
}

async function load(dir = '') {
  curDir = dir;
  contentCont.innerHTML = '<div class="empty-box">Loading...</div>';
  try {
    const res = await fetch('/api/list?dir=' + encodeURIComponent(dir) + '&root=' + root);
    if (!res.ok) throw new Error('Status ' + res.status);
    const data = await res.json();
    allFolders = data.all_folders || [];
    
    if (data.free_gb !== undefined) {
      storageText.textContent = data.free_gb.toFixed(1) + ' GB Free';
    }
    
    renderBreadcrumbs(data.current_dir);
    renderContent(data);
  } catch (e) {
    contentCont.innerHTML = `<div class="empty-box">Failed to load: ${e.message}</div>`;
  }
}

function renderBreadcrumbs(dirStr) {
  if (root === 'screensavers') {
    breadcrumbs.style.display = 'none';
    return;
  }
  breadcrumbs.style.display = 'flex';
  let html = `<span class="crumb ${!dirStr ? 'current' : ''}" onclick="load('')">
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="m3 9 9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><polyline points="9 22 9 12 15 12 15 22"/></svg>
    documents
  </span>`;
  if (dirStr) {
    const parts = dirStr.split('/');
    let accum = '';
    for (let i = 0; i < parts.length; i++) {
      accum += (i > 0 ? '/' : '') + parts[i];
      const target = accum;
      const isLast = (i === parts.length - 1);
      html += `<span style="color:var(--text-dim)">/</span>`;
      html += `<span class="crumb ${isLast ? 'current' : ''}" ${!isLast ? `onclick="load('${target.replace(/'/g, "\\'")}')"` : ''}>${escapeHtml(parts[i])}</span>`;
    }
  }
  breadcrumbs.innerHTML = html;
}

function renderContent(data) {
  const folders = data.folders || [];
  const files = data.files || [];
  
  if (folders.length === 0 && files.length === 0) {
    contentCont.innerHTML = `<div class="empty-box">No ${root === 'screensavers' ? 'screensavers' : 'books'} found in this directory.</div>`;
    return;
  }

  let html = '';

  // Screensavers Grid View
  if (root === 'screensavers') {
    html += `<div class="screensavers-grid" id="items-grid">`;
    for (const file of files) {
      const fileUrl = '/api/file?root=' + root + '&dir=' + encodeURIComponent(curDir) + '&name=' + encodeURIComponent(file.name);
      html += `
      <div class="poster-card item-card" data-name="${escapeHtml(file.name.toLowerCase())}">
        <div class="poster-img-wrap" onclick="openPreview('${fileUrl.replace(/'/g, "\\'")}', '${escapeHtml(file.name).replace(/'/g, "\\'")}')">
          <img class="poster-img" src="${fileUrl}" alt="${escapeHtml(file.name)}" loading="lazy">
          <div class="poster-overlay"></div>
        </div>
        <div class="poster-body">
          <div class="poster-info">
            <div class="poster-name" title="${escapeHtml(file.name)}">${escapeHtml(file.name)}</div>
            <div class="poster-meta">${formatBytes(file.size)} · ${file.ext.toUpperCase()}</div>
          </div>
          <a class="icon-btn" href="${fileUrl}" download="${escapeHtml(file.name)}" title="Download">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" x2="12" y1="15" y2="3"/></svg>
          </a>
          <button class="icon-btn btn-delete" onclick="openDelete('${escapeHtml(file.name).replace(/'/g, "\\'")}', false)" title="Delete">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/></svg>
          </button>
        </div>
      </div>`;
    }
    html += `</div>`;
    contentCont.innerHTML = html;
    filterCards();
    return;
  }

  // Books View: Folders + Table
  if (folders.length > 0) {
    html += `<div class="section-label">Folders (${folders.length})</div><div class="folders-grid">`;
    for (const f of folders) {
      const nextDir = curDir ? curDir + '/' + f : f;
      html += `
      <div class="folder-pill item-card" data-name="${escapeHtml(f.toLowerCase())}">
        <div style="display:flex;align-items:center;gap:10px;flex:1;min-width:0" onclick="load('${nextDir.replace(/'/g, "\\'")}')">
          <svg class="folder-icon" width="18" height="18" viewBox="0 0 24 24" fill="currentColor"><path d="M20 6h-8l-2-2H4c-1.1 0-1.99.9-1.99 2L2 18c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2zm0 12H4V8h16v10z"/></svg>
          <span class="folder-name">${escapeHtml(f)}</span>
        </div>
        <button class="icon-btn btn-delete" style="width:26px;height:26px" onclick="event.stopPropagation(); openDelete('${escapeHtml(f).replace(/'/g, "\\'")}', true)" title="Delete Folder">
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/></svg>
        </button>
      </div>`;
    }
    html += `</div>`;
  }

  if (files.length > 0) {
    html += `<div class="section-label">Books (${files.length})</div><div class="books-table" id="items-table">`;
    for (const file of files) {
      const fileUrl = '/api/file?root=' + root + '&dir=' + encodeURIComponent(curDir) + '&name=' + encodeURIComponent(file.name);
      let badgeClass = '';
      if (file.ext === 'pdf') badgeClass = 'pdf';
      else if (file.ext === 'cbz') badgeClass = 'cbz';

      html += `
      <div class="book-row item-card" data-name="${escapeHtml(file.name.toLowerCase())}">
        <div class="book-left">
          <div class="format-badge ${badgeClass}">${file.ext.toUpperCase()}</div>
          <div class="book-meta-box">
            <div class="book-title" title="${escapeHtml(file.name)}">${escapeHtml(file.name)}</div>
            <div class="book-size">${formatBytes(file.size)}</div>
          </div>
        </div>
        <div class="book-actions">
          <a class="icon-btn" href="${fileUrl}" download="${escapeHtml(file.name)}" title="Download">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" x2="12" y1="15" y2="3"/></svg>
          </a>
          <button class="icon-btn" onclick="openMove('${escapeHtml(file.name).replace(/'/g, "\\'")}')" title="Move to folder">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/><line x1="12" y1="11" x2="12" y2="17"/><line x1="9" y1="14" x2="15" y2="14"/></svg>
          </button>
          <button class="icon-btn btn-delete" onclick="openDelete('${escapeHtml(file.name).replace(/'/g, "\\'")}', false)" title="Delete">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/></svg>
          </button>
        </div>
      </div>`;
    }
    html += `</div>`;
  }

  contentCont.innerHTML = html;
  filterCards();
}

function filterCards() {
  const query = searchInput.value.trim().toLowerCase();
  const items = document.querySelectorAll('.item-card');
  items.forEach(el => {
    const name = el.getAttribute('data-name') || '';
    el.style.display = name.includes(query) ? '' : 'none';
  });
}

function escapeHtml(s) {
  return (s || '').replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

/* Modals */
function openModal(id) { document.getElementById(id).classList.add('active'); }
function closeModal(id) { document.getElementById(id).classList.remove('active'); }

function openMkdir() {
  document.getElementById('mkdir-name').value = '';
  openModal('mkdir-modal');
  setTimeout(() => document.getElementById('mkdir-name').focus(), 50);
}

async function submitMkdir() {
  const name = document.getElementById('mkdir-name').value.trim();
  if (!name) return;
  closeModal('mkdir-modal');
  try {
    const res = await fetch('/api/mkdir?dir=' + encodeURIComponent(curDir) + '&name=' + encodeURIComponent(name) + '&root=' + root, { method: 'POST' });
    if (!res.ok) {
      const err = await res.json();
      throw new Error(err.error || 'Failed to create folder');
    }
    showToast(`Created folder "${name}"`, 'success');
    load(curDir);
  } catch (e) {
    showToast(e.message, 'error');
  }
}

function openMove(fileName) {
  itemToMove = fileName;
  document.getElementById('move-prompt').textContent = `Select destination for "${fileName}":`;
  const sel = document.getElementById('move-target');
  sel.innerHTML = '';
  const rootOpt = document.createElement('option');
  rootOpt.value = '';
  rootOpt.textContent = '📁 Root (documents)';
  if (curDir === '') rootOpt.disabled = true;
  sel.appendChild(rootOpt);

  for (const f of allFolders) {
    const opt = document.createElement('option');
    opt.value = f;
    opt.textContent = '📁 ' + f;
    if (curDir === f) opt.disabled = true;
    sel.appendChild(opt);
  }
  openModal('move-modal');
}

async function submitMove() {
  const dst = document.getElementById('move-target').value;
  if (itemToMove === null) return;
  closeModal('move-modal');
  try {
    const res = await fetch('/api/move?src_dir=' + encodeURIComponent(curDir) + '&name=' + encodeURIComponent(itemToMove) + '&dst_dir=' + encodeURIComponent(dst) + '&root=' + root, { method: 'POST' });
    if (!res.ok) {
      const err = await res.json();
      throw new Error(err.error || 'Failed to move item');
    }
    showToast(`Moved "${itemToMove}"`, 'success');
    load(curDir);
  } catch (e) {
    showToast(e.message, 'error');
  }
}

function openDelete(name, isFolder) {
  itemToDelete = { name, isFolder };
  document.getElementById('delete-prompt').textContent = `Are you sure you want to delete ${isFolder ? 'folder' : ''} "${name}"? This cannot be undone.`;
  openModal('delete-modal');
}

async function submitDelete() {
  if (!itemToDelete) return;
  const { name, isFolder } = itemToDelete;
  closeModal('delete-modal');
  try {
    const res = await fetch('/api/delete?dir=' + encodeURIComponent(curDir) + '&name=' + encodeURIComponent(name) + '&root=' + root, { method: 'POST' });
    if (!res.ok) {
      const err = await res.json();
      throw new Error(err.error || 'Failed to delete item');
    }
    showToast(`Deleted "${name}"`, 'success');
    load(curDir);
  } catch (e) {
    showToast(e.message, 'error');
  }
}

function openPreview(url, name) {
  document.getElementById('preview-img').src = url;
  document.getElementById('preview-title').textContent = name;
  const dl = document.getElementById('preview-dl');
  dl.href = url;
  dl.download = name;
  openModal('preview-modal');
}

/* Upload & Drag Drop */
let dragCounter = 0;
window.addEventListener('dragenter', e => {
  e.preventDefault();
  dragCounter++;
  if (e.dataTransfer.types.includes('Files')) dragOverlay.classList.add('active');
});
window.addEventListener('dragleave', e => {
  e.preventDefault();
  dragCounter--;
  if (dragCounter <= 0) {
    dragCounter = 0;
    dragOverlay.classList.remove('active');
  }
});
window.addEventListener('dragover', e => { e.preventDefault(); });
window.addEventListener('drop', e => {
  e.preventDefault();
  dragCounter = 0;
  dragOverlay.classList.remove('active');
  if (e.dataTransfer.files.length) upload(e.dataTransfer.files);
});

dz.addEventListener('dragover', e => { e.preventDefault(); dz.classList.add('dragover'); });
dz.addEventListener('dragleave', () => dz.classList.remove('dragover'));
dz.addEventListener('drop', e => { e.preventDefault(); dz.classList.remove('dragover'); });

f.onchange = () => { if (f.files.length) upload(f.files); f.value = ''; };

async function upload(files) {
  hud.style.display = 'flex';
  let successCount = 0;
  for (let i = 0; i < files.length; i++) {
    const file = files[i];
    hudTitle.textContent = `Uploading (${i + 1}/${files.length}): ${file.name}`;
    hudPct.textContent = '0%';
    hudFill.style.width = '0%';
    try {
      await new Promise((resolve, reject) => {
        const xhr = new XMLHttpRequest();
        xhr.open('POST', '/upload?dir=' + encodeURIComponent(curDir) + '&name=' + encodeURIComponent(file.name) + '&root=' + root);
        xhr.upload.onprogress = e => {
          if (e.lengthComputable) {
            const pct = Math.round(e.loaded / e.total * 100);
            hudFill.style.width = pct + '%';
            hudPct.textContent = pct + '%';
          }
        };
        xhr.onload = () => { if (xhr.status === 200) resolve(); else reject(xhr.responseText || 'Upload failed'); };
        xhr.onerror = () => reject('Network error');
        xhr.send(file);
      });
      successCount++;
    } catch (e) {
      showToast(`Failed "${file.name}": ${e}`, 'error');
    }
  }
  hudFill.style.width = '100%';
  hudPct.textContent = '100%';
  hudTitle.textContent = 'Upload complete';
  if (successCount > 0) {
    showToast(`Successfully uploaded ${successCount} file(s)`, 'success');
  }
  setTimeout(() => {
    hud.style.display = 'none';
    load(curDir);
  }, 900);
}

load();
</script>
</body>
</html>"#;

// ---- server -------------------------------------------------------------

pub struct ReceiveServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    /// "name (x.x MB)" of the last completed delivery, for the screen.
    last: Arc<Mutex<Option<String>>>,
    received: Arc<AtomicUsize>,
}

impl ReceiveServer {
    /// Bind 8080 when free (friendly for repeat visits), else an ephemeral
    /// port — the QR always carries the real one. `stop` is shared with the
    /// screen so shutdown works even while setup is still running.
    fn start(stop: Arc<AtomicBool>) -> Option<ReceiveServer> {
        if stop.load(Ordering::Relaxed) {
            plog("receive: setup aborted before bind");
            return None;
        }
        let listener = TcpListener::bind(("0.0.0.0", PORT))
            .or_else(|_| TcpListener::bind(("0.0.0.0", 0)))
            .ok()?;
        let port = listener.local_addr().ok()?.port();
        listener.set_nonblocking(true).ok()?;
        firewall_port(port, true);

        let last = Arc::new(Mutex::new(None));
        let received = Arc::new(AtomicUsize::new(0));
        let last_t = Arc::clone(&last);
        let recv_t = Arc::clone(&received);
        let stop_t = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            while !stop_t.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        handle_conn(stream, &last_t, &recv_t);
                    }
                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    Err(e) => {
                        plog(&format!("receive: accept failed, listener stopping: {}", e));
                        break;
                    }
                }
            }
            firewall_port(port, false);
            plog("receive: listener stopped");
        });

        Some(ReceiveServer {
            port,
            stop,
            handle: Some(handle),
            last,
            received,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn status(&self) -> (usize, Option<String>) {
        (
            self.received.load(Ordering::Relaxed),
            self.last.lock().unwrap().clone(),
        )
    }

    fn shutdown(&mut self) {
        firewall_port(self.port, false);
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take();
    }
}

/// Ports this process currently holds an INPUT/ACCEPT rule open for.
static OPEN_RULES: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());

fn track_rule(port: u16, open: bool) {
    if let Ok(mut v) = OPEN_RULES.lock() {
        if open {
            if !v.contains(&port) {
                v.push(port);
            }
        } else {
            v.retain(|&p| p != port);
        }
    }
}

fn firewall_port(port: u16, open: bool) {
    let port_str = port.to_string();
    let spec = [
        "-i".as_ref(),
        "wlan0".as_ref(),
        "-p",
        "tcp",
        "--dport",
        port_str.as_str(),
        "-j",
        "ACCEPT",
    ];
    if open {
        let exists = Command::new(IPTABLES)
            .args(["-C", "INPUT"])
            .args(spec)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !exists {
            let ok = Command::new(IPTABLES)
                .args(["-I", "INPUT", "1"])
                .args(spec)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                track_rule(port, true);
            } else {
                plog("receive: could not open firewall port — uploads will time out");
            }
        } else {
            track_rule(port, true);
        }
    } else {
        track_rule(port, false);
        let _ = Command::new(IPTABLES)
            .args(["-D", "INPUT"])
            .args(spec)
            .status();
    }
}

fn local_ip() -> Option<String> {
    let s = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    s.connect(("8.8.8.8", 53)).ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

pub fn emergency_cleanup() {
    let ports: Vec<u16> = OPEN_RULES
        .lock()
        .map(|v| v.as_slice().to_vec())
        .unwrap_or_default();
    for p in ports {
        firewall_port(p, false);
    }
    firewall_port(PORT, false);
    if let Ok(rd) = std::fs::read_dir(save_dir()) {
        for e in rd.flatten() {
            if e.path().extension().and_then(|x| x.to_str()) == Some("part") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn sanitize_rel_dir(d: &str) -> Option<std::path::PathBuf> {
    let trimmed = d.trim_matches(|c| c == '/' || c == '\\' || c == ' ');
    if trimmed.is_empty() {
        return Some(std::path::PathBuf::new());
    }
    let mut clean = std::path::PathBuf::new();
    for comp in std::path::Path::new(trimmed).components() {
        match comp {
            std::path::Component::Normal(c) => {
                let s = c.to_string_lossy();
                if s.contains('\\') || s.contains('\0') || s == ".." || s == "." {
                    return None;
                }
                clean.push(c);
            }
            _ => return None,
        }
    }
    Some(clean)
}

fn sanitize_folder_name(name: &str) -> Option<String> {
    let s = name.trim();
    if s.is_empty()
        || s.len() > 100
        || s.contains('/')
        || s.contains('\\')
        || s.contains('\0')
        || s.starts_with('.')
        || s == ".."
    {
        return None;
    }
    Some(s.to_string())
}

fn query_param(query: &str, key: &str) -> Option<String> {
    let prefix = format!("{}=", key);
    for part in query.split('&') {
        if let Some(val) = part.strip_prefix(&prefix) {
            return Some(urldecode(val));
        }
    }
    None
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn collect_all_folders(base: &std::path::Path, rel: &std::path::Path, out: &mut Vec<String>) {
    let full = base.join(rel);
    if let Ok(rd) = std::fs::read_dir(full) {
        let mut subdirs = Vec::new();
        for e in rd.flatten() {
            if let Ok(ft) = e.file_type() {
                if ft.is_dir() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !name.starts_with('.') && !name.ends_with(".sdr") {
                        subdirs.push(name);
                    }
                }
            }
        }
        subdirs.sort();
        for sub in subdirs {
            let next_rel = if rel.as_os_str().is_empty() {
                sub.clone()
            } else {
                format!("{}/{}", rel.to_string_lossy(), sub)
            };
            out.push(next_rel.clone());
            collect_all_folders(base, std::path::Path::new(&next_rel), out);
        }
    }
}

fn handle_conn(
    mut stream: TcpStream,
    last: &Arc<Mutex<Option<String>>>,
    received: &Arc<AtomicUsize>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let hdr_started = Instant::now();
    let hdr_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > HDR_CAP {
            respond(
                &mut stream,
                431,
                "Request Header Fields Too Large",
                "text/plain",
                "too large",
            );
            return;
        }
        if hdr_started.elapsed() > HEADER_PHASE_CAP {
            plog("receive: header phase exceeded cap — dropping slow client");
            return;
        }
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };

    let head = String::from_utf8_lossy(&buf[..hdr_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut parts = lines.next().unwrap_or("").split(' ');
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    let mut content_length: Option<u64> = None;
    let mut expect_continue = false;
    for line in lines {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        if k == "content-length" {
            content_length = v.trim().parse().ok();
        } else if k == "expect" && v.trim().eq_ignore_ascii_case("100-continue") {
            expect_continue = true;
        }
    }

    let (raw_path, query_str) = path.split_once('?').unwrap_or((path, ""));

    if method == "GET" {
        if raw_path == "/api/list" || raw_path == "/api/tree" {
            let Some(base) = base_dir(root_param(query_str).as_deref()) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad root\"}",
                );
                return;
            };
            let base_dir = std::path::PathBuf::from(base);
            let rel_str = query_param(query_str, "dir").unwrap_or_default();
            let Some(rel_path) = sanitize_rel_dir(&rel_str) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad dir\"}",
                );
                return;
            };
            let target_dir = base_dir.join(&rel_path);
            if !target_dir.exists() || !target_dir.is_dir() {
                respond(
                    &mut stream,
                    404,
                    "Not Found",
                    "application/json",
                    "{\"error\":\"not found\"}",
                );
                return;
            }

            let mut folders = Vec::new();
            let mut files = Vec::new();
            let ss_root = root_param(query_str).as_deref() == Some("screensavers");

            if let Ok(rd) = std::fs::read_dir(&target_dir) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.')
                        || name.ends_with(".sdr")
                        || SYSTEM_FILES.contains(&name.as_str())
                    {
                        continue;
                    }
                    if let Ok(ft) = e.file_type() {
                        if ft.is_dir() {
                            folders.push(name);
                        } else if ft.is_file() {
                            let ext = e
                                .path()
                                .extension()
                                .map(|x| x.to_string_lossy().to_ascii_lowercase())
                                .unwrap_or_default();
                            let ok = if ss_root {
                                SS_EXTS.contains(&ext.as_str())
                            } else {
                                OK_EXTS.contains(&ext.as_str())
                            };
                            if ok {
                                let sz = e.metadata().map(|m| m.len()).unwrap_or(0);
                                files.push((name, sz, ext));
                            }
                        }
                    }
                }
            }
            folders.sort();
            files.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

            let mut all_folders = Vec::new();
            collect_all_folders(&base_dir, std::path::Path::new(""), &mut all_folders);

            let cur_rel_str = rel_path.to_string_lossy().into_owned();
            let mut json = String::new();
            json.push_str("{\"current_dir\":\"");
            json.push_str(&escape_json(&cur_rel_str));
            json.push_str("\",\"folders\":[");
            for (i, f) in folders.iter().enumerate() {
                if i > 0 {
                    json.push(',');
                }
                json.push('"');
                json.push_str(&escape_json(f));
                json.push('"');
            }
            json.push_str("],\"files\":[");
            for (i, (n, sz, ext)) in files.iter().enumerate() {
                if i > 0 {
                    json.push(',');
                }
                json.push_str("{\"name\":\"");
                json.push_str(&escape_json(n));
                json.push_str("\",\"size\":");
                json.push_str(&sz.to_string());
                json.push_str(",\"ext\":\"");
                json.push_str(&escape_json(ext));
                json.push_str("\"}");
            }
            json.push_str("],\"all_folders\":[");
            for (i, f) in all_folders.iter().enumerate() {
                if i > 0 {
                    json.push(',');
                }
                json.push('"');
                json.push_str(&escape_json(f));
                json.push('"');
            }
            let free_gb = ybdev::sysinfo::storage_free_gb().unwrap_or(0.0);
            json.push_str(&format!("],\"free_gb\":{:.2}", free_gb));
            json.push('}');

            respond(
                &mut stream,
                200,
                "OK",
                "application/json; charset=utf-8",
                &json,
            );
            return;
        }

        if raw_path == "/api/file" || raw_path == "/api/raw" {
            let Some(base) = base_dir(root_param(query_str).as_deref()) else {
                respond(&mut stream, 400, "Bad Request", "text/plain", "bad root");
                return;
            };
            let base_dir = std::path::PathBuf::from(base);
            let rel_str = query_param(query_str, "dir").unwrap_or_default();
            let name = query_param(query_str, "name").unwrap_or_default();
            let Some(rel_path) = sanitize_rel_dir(&rel_str) else {
                respond(&mut stream, 400, "Bad Request", "text/plain", "bad dir");
                return;
            };
            if name.is_empty() || name.contains('/') || name.contains('\\') || name == ".." {
                respond(&mut stream, 400, "Bad Request", "text/plain", "bad name");
                return;
            }
            let target = base_dir.join(rel_path).join(&name);
            if !target.exists() || !target.is_file() {
                respond(&mut stream, 404, "Not Found", "text/plain", "not found");
                return;
            }
            let ext = target
                .extension()
                .and_then(|x| x.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let ctype = match ext.as_str() {
                "jpg" | "jpeg" => "image/jpeg",
                "png" => "image/png",
                "epub" => "application/epub+zip",
                "pdf" => "application/pdf",
                "txt" => "text/plain; charset=utf-8",
                "cbz" => "application/vnd.comicbook+zip",
                "fb2" => "application/x-fictionbook+xml",
                _ => "application/octet-stream",
            };
            if let Ok(mut f) = File::open(&target) {
                let sz = f.metadata().map(|m| m.len()).unwrap_or(0);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: public, max-age=3600\r\nConnection: close\r\n\r\n",
                    ctype, sz
                );
                let _ = stream.write_all(head.as_bytes());
                let mut buf = [0u8; 65536];
                while let Ok(n) = f.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    if stream.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
                let _ = stream.flush();
            } else {
                respond(
                    &mut stream,
                    500,
                    "Server Error",
                    "text/plain",
                    "cannot read file",
                );
            }
            return;
        }

        respond(&mut stream, 200, "OK", "text/html; charset=utf-8", PAGE);
        return;
    }

    if method == "POST" {
        if raw_path == "/api/mkdir" {
            let root = root_param(query_str);
            // One flat image dir — folders don't apply.
            if root.as_deref() == Some("screensavers") {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"no folders in screensavers\"}",
                );
                return;
            }
            let Some(base) = base_dir(root.as_deref()) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad root\"}",
                );
                return;
            };
            let base_dir = std::path::PathBuf::from(base);
            let rel_str = query_param(query_str, "dir").unwrap_or_default();
            let name = query_param(query_str, "name").unwrap_or_default();
            let Some(rel_path) = sanitize_rel_dir(&rel_str) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad dir\"}",
                );
                return;
            };
            let Some(clean_name) = sanitize_folder_name(&name) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad folder name\"}",
                );
                return;
            };
            let target = base_dir.join(rel_path).join(clean_name);
            match std::fs::create_dir_all(&target) {
                Ok(()) => respond(&mut stream, 200, "OK", "application/json", "{\"ok\":true}"),
                Err(e) => respond(
                    &mut stream,
                    500,
                    "Server Error",
                    "application/json",
                    &format!("{{\"error\":\"{}\"}}", e),
                ),
            }
            return;
        }

        if raw_path == "/api/move" {
            let root = root_param(query_str);
            if root.as_deref() == Some("screensavers") {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"no move in screensavers\"}",
                );
                return;
            }
            let Some(base) = base_dir(root.as_deref()) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad root\"}",
                );
                return;
            };
            let base_dir = std::path::PathBuf::from(base);
            let src_str = query_param(query_str, "src_dir").unwrap_or_default();
            let name = query_param(query_str, "name").unwrap_or_default();
            let dst_str = query_param(query_str, "dst_dir").unwrap_or_default();

            let Some(src_rel) = sanitize_rel_dir(&src_str) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad src_dir\"}",
                );
                return;
            };
            let Some(dst_rel) = sanitize_rel_dir(&dst_str) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad dst_dir\"}",
                );
                return;
            };
            if name.is_empty() || name.contains('/') || name.contains('\\') || name == ".." {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad name\"}",
                );
                return;
            }

            let src_path = base_dir.join(src_rel).join(&name);
            let dst_dir = base_dir.join(&dst_rel);
            let dst_path = dst_dir.join(&name);

            if !src_path.exists() {
                respond(
                    &mut stream,
                    404,
                    "Not Found",
                    "application/json",
                    "{\"error\":\"source not found\"}",
                );
                return;
            }
            if dst_path.exists() {
                respond(
                    &mut stream,
                    409,
                    "Conflict",
                    "application/json",
                    "{\"error\":\"destination already exists\"}",
                );
                return;
            }
            let _ = std::fs::create_dir_all(&dst_dir);
            match std::fs::rename(&src_path, &dst_path) {
                Ok(()) => respond(&mut stream, 200, "OK", "application/json", "{\"ok\":true}"),
                Err(e) => respond(
                    &mut stream,
                    500,
                    "Server Error",
                    "application/json",
                    &format!("{{\"error\":\"{}\"}}", e),
                ),
            }
            return;
        }

        if raw_path == "/api/delete" {
            let Some(base) = base_dir(root_param(query_str).as_deref()) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad root\"}",
                );
                return;
            };
            let base_dir = std::path::PathBuf::from(base);
            let rel_str = query_param(query_str, "dir").unwrap_or_default();
            let name = query_param(query_str, "name").unwrap_or_default();

            let Some(rel_path) = sanitize_rel_dir(&rel_str) else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad dir\"}",
                );
                return;
            };
            if name.is_empty() || name.contains('/') || name.contains('\\') || name == ".." {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    "{\"error\":\"bad name\"}",
                );
                return;
            }
            let target = base_dir.join(rel_path).join(&name);
            if !target.exists() {
                respond(
                    &mut stream,
                    404,
                    "Not Found",
                    "application/json",
                    "{\"error\":\"not found\"}",
                );
                return;
            }
            let res = if target.is_dir() {
                std::fs::remove_dir_all(&target)
            } else {
                std::fs::remove_file(&target)
            };
            match res {
                Ok(()) => respond(&mut stream, 200, "OK", "application/json", "{\"ok\":true}"),
                Err(e) => respond(
                    &mut stream,
                    500,
                    "Server Error",
                    "application/json",
                    &format!("{{\"error\":\"{}\"}}", e),
                ),
            }
            return;
        }

        if raw_path == "/upload" {
            let Some(len) = content_length else {
                respond(
                    &mut stream,
                    411,
                    "Length Required",
                    "text/plain",
                    "Content-Length required",
                );
                return;
            };
            if len > MAX_BODY {
                respond(
                    &mut stream,
                    413,
                    "Payload Too Large",
                    "text/plain",
                    "too large",
                );
                return;
            }

            let root = root_param(query_str);
            let Some(base) = base_dir(root.as_deref()) else {
                respond(&mut stream, 400, "Bad Request", "text/plain", "bad root");
                return;
            };
            let ss_root = root.as_deref() == Some("screensavers");

            let rel_dir = query_param(query_str, "dir").unwrap_or_default();
            let Some(clean_rel) = sanitize_rel_dir(&rel_dir) else {
                respond(&mut stream, 400, "Bad Request", "text/plain", "bad dir");
                return;
            };

            let name = query_param(query_str, "name").and_then(|n| sanitize_fetch_name(&n));
            let Some(name) = name else {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    "text/plain",
                    "bad file name",
                );
                return;
            };
            // The allowlist is per-root: books in documents, exactly the
            // renderable image types in screensavers.
            let allow: &[&str] = if ss_root { &SS_EXTS } else { &OK_EXTS };
            let ext_ok = name
                .rsplit('.')
                .next()
                .map(|e| allow.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false);
            if !ext_ok || name.len() > 200 {
                respond(
                    &mut stream,
                    415,
                    "Unsupported Media Type",
                    "text/plain",
                    "unsupported file type",
                );
                return;
            }

            let dir = std::path::PathBuf::from(&base).join(clean_rel);
            let _ = std::fs::create_dir_all(&dir);
            let final_path = dir.join(&name);
            let part_path = dir.join(format!("{}.part", name));
            let final_str = final_path.to_string_lossy().into_owned();
            let part_str = part_path.to_string_lossy().into_owned();

            if final_path.exists() {
                plog(&format!("receive: REFUSED {} — already exists", name));
                respond(
                    &mut stream,
                    409,
                    "Conflict",
                    "text/plain",
                    "file already exists\n",
                );
                return;
            }
            if expect_continue {
                let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
            }
            let body_prefix = buf[hdr_end + 4..].to_vec();
            let t0 = now_ms();

            match write_body(&mut stream, &body_prefix, len, &part_str, &final_str) {
                Ok(()) => {
                    let msg = format!("{} ({:.1} MB)", name, len as f64 / 1048576.0);
                    *last.lock().unwrap() = Some(msg.clone());
                    received.fetch_add(1, Ordering::Relaxed);
                    plog(&format!(
                        "receive: saved {} in {:.1}s",
                        msg,
                        (now_ms() - t0) as f64 / 1000.0
                    ));
                    respond(
                        &mut stream,
                        200,
                        "OK",
                        "text/plain",
                        &format!("saved: {}\n", msg),
                    );
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&part_str);
                    plog(&format!("receive: FAILED {} — {}", name, e));
                    respond(
                        &mut stream,
                        500,
                        "Server Error",
                        "text/plain",
                        &format!("failed: {}", e),
                    );
                }
            }
            return;
        }
    }

    respond(&mut stream, 404, "Not Found", "text/plain", "not found");
}

/// Socket → disk in 64 KB chunks: the file is never held in RAM (the device
/// has ~150 MB free; a big PDF must not OOM it). Counts as delivered only
/// after fsync + rename, exactly like fetch.rs's .part discipline.
fn write_body(
    stream: &mut TcpStream,
    prefix: &[u8],
    len: u64,
    part: &str,
    final_path: &str,
) -> Result<(), String> {
    // O_NOFOLLOW: the HTTP side can't plant symlinks, but another local
    // process could pre-create `<name>.part` as one and make us write
    // through it. Fail with ELOOP instead of following.
    let c_part = std::ffi::CString::new(part).map_err(|_| "bad part path".to_string())?;
    let fd = unsafe {
        libc::open(
            c_part.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW,
            0o644,
        )
    };
    if fd < 0 {
        return Err(format!("create: {}", std::io::Error::last_os_error()));
    }
    let mut out = unsafe { File::from_raw_fd(fd) };
    let mut remaining = len;
    let mut pos = 0usize;
    let started = Instant::now();
    while remaining > 0 {
        if started.elapsed() > BODY_PHASE_CAP {
            // Caller's Err path removes the .part; the socket just dies.
            return Err("upload exceeded time cap".into());
        }
        let take = remaining.min((prefix.len() - pos) as u64) as usize;
        if take > 0 {
            out.write_all(&prefix[pos..pos + take])
                .map_err(|e| format!("write: {}", e))?;
            pos += take;
            remaining -= take as u64;
            continue;
        }
        let mut chunk = [0u8; 65536];
        let n = stream
            .read(&mut chunk)
            .map_err(|e| format!("read: {}", e))?;
        if n == 0 {
            return Err("truncated upload".into());
        }
        out.write_all(&chunk[..n])
            .map_err(|e| format!("write: {}", e))?;
        remaining -= n as u64;
    }
    out.sync_all().map_err(|e| format!("fsync: {}", e))?;
    drop(out);
    // Final guard before the swap: the early exists() check happened
    // before the body arrived; re-check at commit time so the window is
    // as close to zero as the single-threaded accept loop allows.
    if std::path::Path::new(final_path).exists() {
        let _ = std::fs::remove_file(part);
        return Err("file appeared during upload".into());
    }
    std::fs::rename(part, final_path).map_err(|e| format!("rename: {}", e))?;
    Ok(())
}

fn respond(stream: &mut TcpStream, code: u16, reason: &str, ctype: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        code,
        reason,
        ctype,
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn current_ssid() -> Option<String> {
    if let Ok(out) = Command::new("lipc-get-prop")
        .args(["-i", "com.lab126.wifid", "essid"])
        .output()
    {
        if out.status.success() {
            if let Ok(s) = String::from_utf8(out.stdout) {
                let t = s.trim();
                if !t.is_empty() && t != "none" && t != "null" {
                    return Some(t.to_string());
                }
            }
        }
    }
    if let Ok(out) = Command::new("iwgetid").args(["-r"]).output() {
        if out.status.success() {
            if let Ok(s) = String::from_utf8(out.stdout) {
                let t = s.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

// ---- screen ---------------------------------------------------------------

enum Phase {
    /// Wi-Fi + listener setup runs on a thread so the panel still paints.
    Starting,
    Ready {
        url: String,
        ssid: Option<String>,
    },
    Failed(String),
}

pub struct ReceiveScreen {
    phase: Phase,
    stop: Arc<AtomicBool>,
    /// Filled by the setup thread, taken by on_tick.
    setup: Arc<Mutex<Option<Result<(ReceiveServer, String, Option<String>), String>>>>,
    server: Option<ReceiveServer>,
    qr: Option<QrCode>,
    seen_count: usize,
    dims: (i32, i32),
}

impl ReceiveScreen {
    pub fn new() -> ReceiveScreen {
        let stop = Arc::new(AtomicBool::new(false));
        ReceiveScreen {
            phase: Phase::Starting,
            stop: Arc::clone(&stop),
            setup: Arc::new(Mutex::new(None)),
            server: None,
            qr: None,
            seen_count: 0,
            dims: (1236, 1648),
        }
    }

    /// Wi-Fi bring-up can take ~20s on a cold radio; doing it on the UI
    /// thread would freeze the panel mid-Pop. The thread parks its result
    /// in `setup` and on_tick promotes it.
    fn start_setup(&mut self) {
        self.stop.store(false, Ordering::Relaxed);
        self.phase = Phase::Starting;
        *self.setup.lock().unwrap() = None;
        let stop = Arc::clone(&self.stop);
        let slot = Arc::clone(&self.setup);
        std::thread::spawn(move || {
            let r = (|| -> Result<(ReceiveServer, String, Option<String>), String> {
                wifi::ensure_wifi();
                if !wifi::wait_for_wifi(Duration::from_secs(10)) {
                    return Err("Wi-Fi not connected".into());
                }
                let Some(srv) = ReceiveServer::start(Arc::clone(&stop)) else {
                    return Err("cannot bind listener".into());
                };
                // The route can lag association by a moment.
                let mut ip = local_ip();
                for _ in 0..10 {
                    if ip.is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(300));
                    ip = local_ip();
                }
                let url = format!(
                    "http://{}:{}/",
                    ip.unwrap_or_else(|| "127.0.0.1".into()),
                    srv.port()
                );
                let ssid = current_ssid();
                Ok((srv, url, ssid))
            })();
            *slot.lock().unwrap() = Some(r);
        });
    }
}

impl Screen for ReceiveScreen {
    fn default_edges(&self) -> bool {
        true
    }

    fn on_enter(&mut self) -> Action {
        crate::awake::screen_wants_awake(true);
        self.start_setup();
        Action::RedrawFull
    }

    fn on_leave(&mut self) {
        crate::awake::screen_wants_awake(false);
        self.stop.store(true, Ordering::Relaxed);
        if let Some(s) = &mut self.server {
            s.shutdown();
        }
    }

    fn holds_awake(&self) -> bool {
        true
    }

    fn tick_interval(&self) -> Duration {
        Duration::from_millis(300)
    }

    fn on_tick(&mut self) -> Action {
        if matches!(self.phase, Phase::Starting) {
            match self.setup.lock().unwrap().take() {
                Some(Ok((srv, url, ssid))) => {
                    self.qr = QrCode::new(url.as_bytes()).ok();
                    plog(&format!("receive: listening on {}", url));
                    self.phase = Phase::Ready { url, ssid };
                    self.server = Some(srv);
                    Action::RedrawFull
                }
                Some(Err(e)) => {
                    self.phase = Phase::Failed(e);
                    Action::RedrawFull
                }
                None => Action::Keep,
            }
        } else if let Some(srv) = &self.server {
            let (n, _) = srv.status();
            if n != self.seen_count {
                self.seen_count = n;
                return Action::Redraw;
            }
            Action::Keep
        } else {
            Action::Keep
        }
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        self.dims = (w, h);
        p.clear(255);

        let bar_h = pt(40.0);
        p.rect(Rect::new(0, 0, w, bar_h), 245);
        p.hline_t(bar_h, 0, w, 1, 200);
        p.text(pt(16.0), pt(24.0), 10.5, 0, "Receive over Wi-Fi");

        match &self.phase {
            Phase::Starting => {
                p.text_center(h / 2, 11.0, 0, "Turning on Wi-Fi…");
                p.text_center(h / 2 + pt(18.0), 8.5, 130, "the address will appear here");
            }
            Phase::Failed(e) => {
                p.text_center(h / 2, 11.0, 0, e);
                p.text_center(h / 2 + pt(18.0), 9.0, 0, "tap to retry");
            }
            Phase::Ready { url, ssid } => {
                if let Some(ssid_name) = ssid {
                    let s = format!("Wi-Fi: {}", ssid_name);
                    let trunc = p.truncate(8.5, &s, 140.0);
                    p.text_right(w - pt(16.0), pt(24.0), 8.5, 100, &trunc);
                }

                // QR: e-ink's ideal payload — static, pure black/white.
                // 4-module quiet zone, scaled to fit, drawn once.
                let top = bar_h + pt(24.0);
                let target = (w * 2 / 5).min(h - top - pt(230.0));
                if let Some(qr) = &self.qr {
                    let qw = qr.width() as i32;
                    let total = qw + 8;
                    let scale = (target / total).max(2);
                    let size = total * scale;
                    let x0 = (w - size) / 2;
                    p.rect(Rect::new(x0, top, size, size), 255);
                    for my in 0..qw {
                        for mx in 0..qw {
                            if qr[(mx as usize, my as usize)] == Color::Dark {
                                p.rect(
                                    Rect::new(
                                        x0 + (mx + 4) * scale,
                                        top + (my + 4) * scale,
                                        scale,
                                        scale,
                                    ),
                                    0,
                                );
                            }
                        }
                    }

                    let ty = top + size + pt(34.0);
                    p.text_center(ty, 11.5, 0, url);
                    if let Some(s) = ssid {
                        p.text_center(
                            ty + pt(18.0),
                            8.5,
                            110,
                            &format!("connect phone/laptop to “{}”", s),
                        );
                    } else {
                        p.text_center(
                            ty + pt(18.0),
                            8.5,
                            110,
                            "scan QR with camera or open address above",
                        );
                    }
                    p.text_center(
                        ty + pt(32.0),
                        8.5,
                        130,
                        "drop books or screensavers onto the page to send them",
                    );

                    let (n, last) = self
                        .server
                        .as_ref()
                        .map(|s| s.status())
                        .unwrap_or((0, None));
                    let sy = ty + pt(60.0);
                    if n > 0 {
                        let line = format!("received {} — last: {}", n, last.unwrap_or_default());
                        let trunc = p.truncate(9.0, &line, p.width_pt() - 20.0);
                        p.text_center(sy, 9.0, 0, &trunc);
                    } else {
                        p.text_center(sy, 9.0, 130, "waiting for files…");
                    }
                } else {
                    p.text_center(h / 2, 11.0, 0, url);
                }
            }
        }

        p.text_center(
            h - pt(16.0),
            8.5,
            130,
            "Swipe up from bottom right corner to exit",
        );
        let cw = pt(18.0);
        let ch = pt(18.0);
        p.hline_t(h - pt(8.0), w - cw - pt(8.0), w - pt(8.0), 2, 160);
        p.rect(Rect::new(w - pt(10.0), h - ch - pt(8.0), 2, ch), 160);
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (vw, vh) = (self.dims.0 as u32, self.dims.1 as u32);
        if g.corner_back() || g.corner_back_in(vw, vh) {
            return Action::Pop;
        }
        match g {
            Gesture::Tap { .. } | Gesture::TwoFingerTap => {
                if matches!(self.phase, Phase::Failed(_)) {
                    self.start_setup();
                    Action::RedrawFull
                } else {
                    Action::Keep
                }
            }
            _ => Action::Keep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_roundtrip_and_extension_guard() {
        let dir = std::env::temp_dir().join("yb-receive-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("YB_SAVE_DIR", dir.to_str().unwrap());
        let ss_dir = std::env::temp_dir().join("yb-receive-ss-test");
        let _ = std::fs::remove_dir_all(&ss_dir);
        std::fs::create_dir_all(&ss_dir).unwrap();
        std::env::set_var("YB_SS_DIR", ss_dir.to_str().unwrap());

        let stop = Arc::new(AtomicBool::new(false));
        let mut srv = ReceiveServer::start(Arc::clone(&stop)).expect("server");
        let port = srv.port();

        // Good upload: raw body, query-encoded name with a space, split
        // across two writes (headers first, body after).
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body = b"fake epub bytes";
        let req = format!(
            "POST /upload?name=test%20book.epub HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        c.write_all(req.as_bytes()).unwrap();
        c.write_all(body).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "{}",
            String::from_utf8_lossy(&resp)
        );
        assert_eq!(std::fs::read(dir.join("test book.epub")).unwrap(), body);
        assert!(!dir.join("test book.epub.part").exists());

        // Headers + body in ONE packet must work too (prefix path), and a
        // disallowed extension must be refused without leaving a file.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            b"POST /upload?name=evil.sh HTTP/1.1\r\nHost: t\r\nContent-Length: 2\r\n\r\nhi",
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 415"));
        assert!(!dir.join("evil.sh").exists());

        // MOBI is refused for the same reason it's not in the library
        // allowlist: no parser exists, so accepting it would only set
        // up an open error later.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            b"POST /upload?name=book.mobi HTTP/1.1\r\nHost: t\r\nContent-Length: 2\r\n\r\nhi",
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 415"));
        assert!(!dir.join("book.mobi").exists());

        // Path traversal is neutralized by sanitize_fetch_name: the file
        // lands under its basename inside the save dir, never outside it.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"POST /upload?name=..%2F..%2Fescape.epub HTTP/1.1\r\nHost: t\r\nContent-Length: 1\r\n\r\nx")
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "{}",
            String::from_utf8_lossy(&resp)
        );
        assert_eq!(std::fs::read(dir.join("escape.epub")).unwrap(), b"x");
        assert!(!dir.parent().unwrap().join("escape.epub").exists());

        // GET serves the upload page.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"GET / HTTP/1.1\r\nHost: t\r\n\r\n").unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 200"));
        assert!(resp.windows(9).any(|w| w == b"text/html"));

        // Regression for the O_NONBLOCK race: a client that connects and
        // only sends its request a beat later must NOT be treated as dead.
        // Before the set_nonblocking(false) fix, the server's first read
        // returned WouldBlock and closed the connection with no response.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        c.write_all(b"GET / HTTP/1.1\r\nHost: t\r\n\r\n").unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "delayed-request client was dropped: {}",
            String::from_utf8_lossy(&resp)
        );

        let (n, last) = srv.status();
        assert_eq!(n, 2);
        assert!(last.unwrap().contains("escape.epub"));

        // Test /api/mkdir
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(
            b"POST /api/mkdir?name=Sci-Fi HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n",
        )
        .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "mkdir failed: {}",
            String::from_utf8_lossy(&resp)
        );
        assert!(dir.join("Sci-Fi").is_dir());

        // Test /upload with dir parameter
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body2 = b"dune content";
        let req2 = format!(
            "POST /upload?dir=Sci-Fi&name=Dune.epub HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n\r\n",
            body2.len()
        );
        c.write_all(req2.as_bytes()).unwrap();
        c.write_all(body2).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "upload to dir failed: {}",
            String::from_utf8_lossy(&resp)
        );
        assert_eq!(
            std::fs::read(dir.join("Sci-Fi").join("Dune.epub")).unwrap(),
            body2
        );

        // Test /api/list
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"GET /api/list HTTP/1.1\r\nHost: t\r\n\r\n")
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 200"));
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(resp_str.contains("\"folders\":[\"Sci-Fi\"]"));
        assert!(resp_str.contains("\"all_folders\":[\"Sci-Fi\"]"));

        // Test /api/move
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"POST /api/move?src_dir=&name=test%20book.epub&dst_dir=Sci-Fi HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "move failed: {}",
            String::from_utf8_lossy(&resp)
        );
        assert!(dir.join("Sci-Fi").join("test book.epub").exists());
        assert!(!dir.join("test book.epub").exists());

        // Test /api/delete
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"POST /api/delete?dir=Sci-Fi&name=test%20book.epub HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "delete failed: {}",
            String::from_utf8_lossy(&resp)
        );
        assert!(!dir.join("Sci-Fi").join("test book.epub").exists());

        // ---- screensavers root ----------------------------------------
        // A PNG lands in the screensavers dir; an epub is refused there
        // for the same reason mobi is refused in documents: the consumer
        // (the sleep-screen picker) can't render it.
        let png = b"fake png bytes";
        let req = format!(
            "POST /upload?root=screensavers&name=cover.png HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n\r\n",
            png.len()
        );
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(req.as_bytes()).unwrap();
        c.write_all(png).unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "ss png upload: {}",
            String::from_utf8_lossy(&resp)
        );
        assert_eq!(std::fs::read(ss_dir.join("cover.png")).unwrap(), png);

        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"POST /upload?root=screensavers&name=book.epub HTTP/1.1\r\nHost: t\r\nContent-Length: 2\r\n\r\nhi")
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 415"),
            "epub must be refused in screensavers: {}",
            String::from_utf8_lossy(&resp)
        );
        assert!(!ss_dir.join("book.epub").exists());

        // Flat root: mkdir and move are refused server-side, not just
        // hidden in the UI.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"POST /api/mkdir?root=screensavers&name=walls HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 400"));
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"POST /api/move?root=screensavers&src_dir=&name=cover.png&dst_dir= HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 400"));

        // List shows the image, hides a macOS AppleDouble sidecar the
        // same way the device scan does, and a bogus root is rejected.
        let _ = std::fs::write(ss_dir.join("._cover.jpg"), b"finder junk");
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"GET /api/list?root=screensavers HTTP/1.1\r\nHost: t\r\n\r\n")
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        let resp_str = String::from_utf8_lossy(&resp);
        assert!(
            resp_str.starts_with("HTTP/1.1 200"),
            "ss list: {}",
            resp_str
        );
        assert!(resp_str.contains("cover.png"));
        assert!(
            !resp_str.contains("_cover.jpg"),
            "AppleDouble sidecar leaked: {}",
            resp_str
        );

        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"GET /api/list?root=/etc HTTP/1.1\r\nHost: t\r\n\r\n")
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 400"),
            "bogus root accepted: {}",
            String::from_utf8_lossy(&resp)
        );

        // Test /api/file download & preview
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"GET /api/file?dir=Sci-Fi&name=Dune.epub HTTP/1.1\r\nHost: t\r\n\r\n")
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(
            resp.starts_with(b"HTTP/1.1 200"),
            "api/file failed: {}",
            String::from_utf8_lossy(&resp)
        );
        assert!(resp.windows(20).any(|w| w == b"application/epub+zip"));

        // Delete works in the screensavers root too.
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"POST /api/delete?root=screensavers&dir=&name=cover.png HTTP/1.1\r\nHost: t\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let mut resp = Vec::new();
        c.read_to_end(&mut resp).unwrap();
        assert!(resp.starts_with(b"HTTP/1.1 200"));
        assert!(!ss_dir.join("cover.png").exists());

        srv.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&ss_dir);
    }

    #[test]
    fn renders_receive_screen_to_png() {
        let font = yui::font::Font::load().unwrap();
        let mut s = ReceiveScreen::new();
        let url = "http://192.168.1.50:8080/".to_string();
        s.qr = QrCode::new(url.as_bytes()).ok();
        s.phase = Phase::Ready {
            url,
            ssid: Some("HomeStudio_5G".to_string()),
        };
        let mut canvas = vec![0u8; 1236 * 1648];
        let mut panel = vec![255u8; 1248 * 1648];
        let mut p = yui::Painter::new(
            &mut panel,
            1236,
            1648,
            1248,
            yui::Orientation::Portrait,
            &mut canvas,
            &font,
        );
        s.draw(&mut p);
        let artifact_path = "/tmp/dev_artifacts/receive_screen_device.png";
        let file = std::fs::File::create(artifact_path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 1236, 1648);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&canvas)
            .unwrap();
    }
}
