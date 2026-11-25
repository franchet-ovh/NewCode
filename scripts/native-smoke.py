#!/usr/bin/env python3
"""Check native X11 idle presentation, keyboard input, saving, and clean shutdown.

Requires an X11 desktop (including Cinnamon), libXdamage, and libXtst. All editor
data is temporary. Build with cargo build --release --bins, then optionally pass
--baseline /path/to/previous/focus-editor to compare redraw and CPU measurements.
"""
import argparse
import ctypes as C
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time


def wait_until(predicate, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError("Timed out waiting for native editor")


class ClientData(C.Union):
    _fields_ = [("longs", C.c_long * 5)]


class ClientMessage(C.Structure):
    _fields_ = [("type", C.c_int), ("serial", C.c_ulong), ("send_event", C.c_int),
                ("display", C.c_void_p), ("window", C.c_ulong), ("message_type", C.c_ulong),
                ("format", C.c_int), ("data", ClientData)]


class XEvent(C.Union):
    _fields_ = [("type", C.c_int), ("client", ClientMessage), ("pad", C.c_long * 24)]


class Desktop:
    def __init__(self):
        self.x = C.CDLL("libX11.so.6")
        self.damage = C.CDLL("libXdamage.so.1")
        self.test = C.CDLL("libXtst.so.6")
        display, window = C.c_void_p, C.c_ulong
        pointer = C.POINTER
        bindings = [
            (self.x, "XOpenDisplay", [C.c_char_p], display),
            (self.x, "XDefaultRootWindow", [display], window),
            (self.x, "XQueryTree", [display, window, pointer(window), pointer(window), pointer(pointer(window)), pointer(C.c_uint)], C.c_int),
            (self.x, "XFetchName", [display, window, pointer(C.c_char_p)], C.c_int),
            (self.x, "XFree", [C.c_void_p], C.c_int),
            (self.x, "XPending", [display], C.c_int),
            (self.x, "XNextEvent", [display, pointer(XEvent)], C.c_int),
            (self.x, "XFlush", [display], C.c_int),
            (self.x, "XSync", [display, C.c_int], C.c_int),
            (self.x, "XCloseDisplay", [display], C.c_int),
            (self.x, "XInternAtom", [display, C.c_char_p, C.c_int], window),
            (self.x, "XSendEvent", [display, window, C.c_int, C.c_long, pointer(XEvent)], C.c_int),
            (self.x, "XRaiseWindow", [display, window], C.c_int),
            (self.x, "XQueryPointer", [display, window, pointer(window), pointer(window), pointer(C.c_int), pointer(C.c_int), pointer(C.c_int), pointer(C.c_int), pointer(C.c_uint)], C.c_int),
            (self.x, "XSetInputFocus", [display, window, C.c_int, C.c_ulong], C.c_int),
            (self.x, "XGetGeometry", [display, window, pointer(window), pointer(C.c_int), pointer(C.c_int), pointer(C.c_uint), pointer(C.c_uint), pointer(C.c_uint), pointer(C.c_uint)], C.c_int),
            (self.x, "XTranslateCoordinates", [display, window, window, C.c_int, C.c_int, pointer(C.c_int), pointer(C.c_int), pointer(window)], C.c_int),
            (self.x, "XStringToKeysym", [C.c_char_p], window),
            (self.x, "XKeysymToKeycode", [display, window], C.c_uint),
            (self.damage, "XDamageQueryExtension", [display, pointer(C.c_int), pointer(C.c_int)], C.c_int),
            (self.damage, "XDamageCreate", [display, window, C.c_int], window),
            (self.damage, "XDamageSubtract", [display, window, window, window], None),
            (self.damage, "XDamageDestroy", [display, window], None),
            (self.test, "XTestFakeMotionEvent", [display, C.c_int, C.c_int, C.c_int, C.c_ulong], C.c_int),
            (self.test, "XTestFakeButtonEvent", [display, C.c_uint, C.c_int, C.c_ulong], C.c_int),
            (self.test, "XTestFakeKeyEvent", [display, C.c_uint, C.c_int, C.c_ulong], C.c_int),
        ]
        for library, name, arguments, result in bindings:
            function = getattr(library, name)
            function.argtypes, function.restype = arguments, result
        self.display = self.x.XOpenDisplay(None)
        assert self.display, "An X11 desktop is required"
        self.root = self.x.XDefaultRootWindow(self.display)
        event, error = C.c_int(), C.c_int()
        assert self.damage.XDamageQueryExtension(self.display, C.byref(event), C.byref(error)), "XDamage is required"
        self.damage_event = event.value

    def find(self, title, window=None):
        window = self.root if window is None else window
        name = C.c_char_p()
        if self.x.XFetchName(self.display, window, C.byref(name)) and name.value:
            value = name.value.decode(errors="replace")
            self.x.XFree(name)
            if value == title:
                return window
        root, parent, count = C.c_ulong(), C.c_ulong(), C.c_uint()
        children = C.POINTER(C.c_ulong)()
        if self.x.XQueryTree(self.display, window, C.byref(root), C.byref(parent), C.byref(children), C.byref(count)):
            ids = list(children[:count.value])
            if children:
                self.x.XFree(children)
            for child in ids:
                found = self.find(title, child)
                if found:
                    return found
        return None

    def measure(self, window, pid, seconds):
        damage = self.damage.XDamageCreate(self.display, window, 3)  # NonEmpty
        count = 0
        before = cpu_ticks(pid)
        compositor = compositor_pids()
        compositor_before = sum(cpu_ticks(pid) for pid in compositor)
        start = time.monotonic()
        event = XEvent()
        try:
            while time.monotonic() - start < seconds:
                while self.x.XPending(self.display):
                    self.x.XNextEvent(self.display, C.byref(event))
                    if event.type == self.damage_event:
                        count += 1
                        self.damage.XDamageSubtract(self.display, damage, 0, 0)
                        self.x.XFlush(self.display)
                time.sleep(0.002)
        finally:
            self.damage.XDamageDestroy(self.display, damage)
        elapsed = time.monotonic() - start
        scale = 100 / os.sysconf("SC_CLK_TCK") / elapsed
        return {"seconds": round(elapsed, 2), "damage_events": count,
                "redraws_per_second": round(count / elapsed, 2),
                "editor_cpu_percent": round((cpu_ticks(pid) - before) * scale, 2),
                "compositor_cpu_percent": round((sum(cpu_ticks(pid) for pid in compositor) - compositor_before) * scale, 2),
                "compositors": list(compositor.values())}

    def key(self, name, pressed):
        symbol = self.x.XStringToKeysym(name.encode())
        code = self.x.XKeysymToKeycode(self.display, symbol)
        assert code, name
        self.test.XTestFakeKeyEvent(self.display, code, pressed, 0)
        self.x.XFlush(self.display)

    def shortcut(self, name):
        self.key("Control_L", True)
        self.key(name, True)
        self.key(name, False)
        self.key("Control_L", False)
        time.sleep(0.15)

    def activate(self, window):
        event = XEvent()
        event.client.type = 33
        event.client.display, event.client.window = self.display, window
        event.client.message_type = self.x.XInternAtom(self.display, b"_NET_ACTIVE_WINDOW", False)
        event.client.format = 32
        event.client.data.longs[0] = 2  # Pager/test activation request.
        self.x.XSendEvent(self.display, self.root, False, (1 << 20) | (1 << 19), C.byref(event))
        self.x.XRaiseWindow(self.display, window)
        self.x.XSetInputFocus(self.display, window, 2, 0)
        self.x.XSync(self.display, False)
        time.sleep(.2)

    def pointer_targets(self, window):
        top = window
        while True:
            root, parent, count = C.c_ulong(), C.c_ulong(), C.c_uint()
            children = C.POINTER(C.c_ulong)()
            if not self.x.XQueryTree(self.display, top, C.byref(root), C.byref(parent), C.byref(children), C.byref(count)):
                return False
            if children:
                self.x.XFree(children)
            if parent.value == self.root:
                break
            if not parent.value:
                return False
            top = parent.value
        root, child = C.c_ulong(), C.c_ulong()
        rx, ry, wx, wy, mask = C.c_int(), C.c_int(), C.c_int(), C.c_int(), C.c_uint()
        self.x.XQueryPointer(self.display, self.root, C.byref(root), C.byref(child), C.byref(rx), C.byref(ry), C.byref(wx), C.byref(wy), C.byref(mask))
        return child.value == top

    def bounds(self, window):
        root = C.c_ulong()
        gx, gy = C.c_int(), C.c_int()
        width, height, border, depth = C.c_uint(), C.c_uint(), C.c_uint(), C.c_uint()
        assert self.x.XGetGeometry(self.display, window, C.byref(root), C.byref(gx), C.byref(gy), C.byref(width), C.byref(height), C.byref(border), C.byref(depth))
        x, y, child = C.c_int(), C.c_int(), C.c_ulong()
        self.x.XTranslateCoordinates(self.display, window, self.root, 0, 0, C.byref(x), C.byref(y), C.byref(child))
        return x.value, y.value, width.value, height.value

    def edit(self, window):
        self.activate(window)
        left, top, width, height = self.bounds(window)
        # Target the first source line; blank space below a short document has no TextEdit response.
        x, y = C.c_int(left + int(width * .65)), C.c_int(top + int(height * .16))
        self.test.XTestFakeMotionEvent(self.display, -1, x.value, y.value, 0)
        self.x.XSync(self.display, False)
        assert self.pointer_targets(window), "Refusing synthetic input: the test window is obscured"
        self.test.XTestFakeButtonEvent(self.display, 1, True, 0)
        self.test.XTestFakeButtonEvent(self.display, 1, False, 0)
        self.x.XFlush(self.display)
        time.sleep(0.2)
        self.shortcut("a")
        for letter in "native smoke":
            key = "space" if letter == " " else letter
            self.key(key, True)
            self.key(key, False)
        time.sleep(0.2)
        self.shortcut("s")

    def close_window(self, window):
        event = XEvent()
        event.client.type = 33  # ClientMessage
        event.client.display, event.client.window = self.display, window
        event.client.message_type = self.x.XInternAtom(self.display, b"WM_PROTOCOLS", False)
        event.client.format = 32
        event.client.data.longs[0] = self.x.XInternAtom(self.display, b"WM_DELETE_WINDOW", False)
        self.x.XSendEvent(self.display, window, False, 0, C.byref(event))
        self.x.XFlush(self.display)


def cpu_ticks(pid):
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return int(fields[11]) + int(fields[12])
    except FileNotFoundError:
        return 0


def compositor_pids():
    found = {}
    for entry in Path("/proc").iterdir():
        if entry.name.isdigit():
            try:
                name = (entry / "comm").read_text().strip()
                if name in {"cinnamon", "muffin", "xfwm4", "gnome-shell", "kwin_x11"}:
                    found[int(entry.name)] = name
            except (FileNotFoundError, PermissionError, ProcessLookupError):
                pass
    return found


def run(binary, desktop, seconds, check_input):
    with tempfile.TemporaryDirectory(prefix="focus-native-smoke-") as temporary:
        root = Path(temporary)
        workspace = root / root.name
        workspace.mkdir()
        source = workspace / "example.txt"
        source.write_text("original\n")
        identity = hashlib.sha256(os.fsencode(workspace)).hexdigest()
        settings = root / "data/focus-editor/workspaces" / identity / "settings.toml"
        settings.parent.mkdir(parents=True)
        quoted = lambda path: json.dumps(str(path))
        settings.write_text(f"workspace = {quoted(workspace)}\nsession_workspace = {quoted(workspace)}\n"
                            f"open_files = [{quoted(source)}]\nactive_file = {quoted(source)}\n")
        environment = dict(os.environ, XDG_DATA_HOME=str(root / "data"), XDG_CONFIG_HOME=str(root / "config"), SHELL="/bin/sh")
        with (root / "app.log").open("w+") as log:
            app = subprocess.Popen([str(binary), "--foreground", "--log-level=off", str(workspace)],
                                   env=environment, stdout=log, stderr=log)
            try:
                window = wait_until(lambda: desktop.find(f"Focus Editor — {workspace.name}"))
                desktop.activate(window)
                time.sleep(3)  # Initial fonts, layout, tooltips, and WM animations.
                assert app.poll() is None, "Native editor exited during startup"
                measurements = desktop.measure(window, app.pid, seconds)
                if check_input:
                    assert measurements["redraws_per_second"] <= 5, measurements
                    desktop.edit(window)
                    wait_until(lambda: source.read_text() == "native smoke")
                desktop.close_window(window)
                app.wait(timeout=10)
                assert app.returncode == 0, app.returncode
                assert not list((root / "data/focus-editor/runtime/instances").glob("*.json")), "Session registration survived close"
                return measurements
            except Exception:
                print("Fixture document:", repr(source.read_text()), flush=True)
                try:
                    from PIL import ImageGrab
                    desktop.activate(window)
                    left, top, width, height = desktop.bounds(window)
                    screenshot = Path("target/native-smoke-failure.png")
                    screenshot.parent.mkdir(parents=True, exist_ok=True)
                    ImageGrab.grab(bbox=(left, top, left+width, top+height), xdisplay=os.environ.get("DISPLAY")).save(screenshot)
                    print("Native failure screenshot:", screenshot, flush=True)
                except Exception as error:
                    print("Could not capture native window:", error, flush=True)
                log.seek(0)
                print(log.read(), flush=True)
                raise
            finally:
                if app.poll() is None:
                    app.terminate()
                    app.wait(timeout=10)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/focus-editor"))
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--seconds", type=float, default=5)
    args = parser.parse_args()
    assert args.seconds > 0
    desktop = Desktop()
    try:
        if args.baseline:
            print("Before:", json.dumps(run(args.baseline.resolve(), desktop, args.seconds, False)), flush=True)
        print("After:", json.dumps(run(args.binary.resolve(), desktop, args.seconds, True)), flush=True)
        print("Native idle presentation, keyboard editing/saving, and clean close passed", flush=True)
    finally:
        desktop.x.XCloseDisplay(desktop.display)
