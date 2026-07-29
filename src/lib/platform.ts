// Lightweight platform detection that is safe during SSR or without navigator.
export const isMac = (): boolean => {
  try {
    const ua = navigator.userAgent || "";
    const plat = (navigator.platform || "").toLowerCase();
    return /mac/i.test(ua) || plat.includes("mac");
  } catch {
    return false;
  }
};

export const isWindows = (): boolean => {
  try {
    const ua = navigator.userAgent || "";
    return /windows|win32|win64/i.test(ua);
  } catch {
    return false;
  }
};

export const isLinux = (): boolean => {
  try {
    const ua = navigator.userAgent || "";
    // WebKitGTK and Chromium user agents usually include Linux or X11 on
    // Linux, Wayland, and X11.
    return (
      /linux|x11/i.test(ua) && !/android/i.test(ua) && !isMac() && !isWindows()
    );
  } catch {
    return false;
  }
};

// Disable all drag regions on Linux to avoid Wayland window-event failures
// involving gtk_window_begin_move_drag (Tauri #13440). Retain existing drag
// behavior on macOS; the Windows application does not depend on it.
//
// These constants are designed for JSX attribute spreading with
// `{...DRAG_REGION_ATTR}`. Wry detects the presence of
// `data-tauri-drag-region`, so disabling it requires omitting the attribute
// entirely; an empty string or "false" still activates it.
export const DRAG_REGION_ENABLED = !isLinux();

export const DRAG_REGION_ATTR: Record<string, unknown> = DRAG_REGION_ENABLED
  ? { "data-tauri-drag-region": true }
  : {};

export const DRAG_REGION_STYLE: Record<string, unknown> = DRAG_REGION_ENABLED
  ? { WebkitAppRegion: "drag" }
  : {};
