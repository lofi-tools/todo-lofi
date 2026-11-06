import json
import os
import re
import signal
import subprocess
import sys
import time
from pathlib import Path

import gi

gi.require_version("IBus", "1.0")
from gi.repository import IBus


def wait_for(label, read, timeout=12):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = read()
        if value is not None:
            return value
        time.sleep(0.1)
    raise RuntimeError(f"Timed out waiting for {label}")


def command(*arguments):
    return subprocess.check_output(arguments, text=True)


def window_position(info):
    return tuple(
        int(re.search(rf"Absolute upper-left {axis}:\s+(-?\d+)", info)[1])
        for axis in "XY"
    )


def candidate_position():
    for line in command("xwininfo", "-root", "-tree").splitlines():
        if '("ibus-ui-gtk3" "Ibus-ui-gtk3")' in line:
            window = re.search(r"0x[0-9a-f]+", line)[0]
            info = command("xwininfo", "-id", window)
            if "Map State: IsViewable" in info:
                return window_position(info)
    return None


def check_composition(log, app, bus, idle_context):
    def event(stage, attempt=None):
        for line in log.read_text().splitlines():
            prefix = "SHEPHERD_TERMINAL_SMOKE "
            if not line.startswith(prefix):
                continue
            value = json.loads(line.removeprefix(prefix))
            if value.get("ok") is False:
                raise RuntimeError(value["error"])
            if stage is None and value.get("ok") is True:
                return value
            if stage is not None and value.get("ime") == stage and value.get("attempt") == attempt:
                return value
        if app.poll() is not None:
            raise RuntimeError(f"The IME smoke app exited before reporting {stage or 'success'}")
        return None

    def focused_pinyin_context():
        context = bus.current_input_context()
        if not context or context == idle_context:
            return None
        engine = bus.get_global_engine()
        if engine is None or engine.get_name() != "libpinyin":
            bus.set_global_engine("libpinyin")
            return None
        return context

    for attempt in (1, 2):
        ready = wait_for("terminal IME anchor", lambda: event("ready", attempt), timeout=45)
        window = command("xdotool", "search", "--onlyvisible", "--name", "^Shepherd$").strip()
        origin = window_position(command("xwininfo", "-id", window))
        scale = ready["devicePixelRatio"]
        caret = tuple(offset + value * scale for offset, value in zip(origin, ready["cursor"]))
        command("xdotool", "windowfocus", "--sync", window)
        wait_for("Pinyin engine in the focused IBus input context", focused_pinyin_context)
        command("xdotool", "key", "--clearmodifiers", "n", "i")

        def panel_at_caret():
            popup = candidate_position()
            if popup and abs(popup[0] - caret[0]) <= 3 and -3 <= popup[1] - caret[1] <= 17 * scale + 3:
                return popup
            return None

        try:
            popup = wait_for("IBus candidate popup at the current terminal caret", panel_at_caret)
        except RuntimeError as failure:
            raise RuntimeError(f"{failure}. Caret {caret}, popup {candidate_position()}") from failure
        print(f"IME caret {caret}, candidate popup {popup}", flush=True)
        if attempt == 2:
            command("xdotool", "key", "--clearmodifiers", "Down")
        command("xdotool", "key", "--clearmodifiers", "space")
        wait_for("trusted Pinyin commit", lambda: event("committed", attempt))
        command("xdotool", "key", "--clearmodifiers", "Return")

    wait_for("matching PTY output from both Pinyin selections", lambda: event(None))


def main(fixture, launch):
    address = fixture / "ibus.sock"
    os.environ.pop("IBUS_USE_PORTAL", None)
    app_environment = {**os.environ, "IBUS_ADDRESS": f"unix:path={address}"}
    os.environ.update(
        IBUS_ADDRESS=app_environment["IBUS_ADDRESS"],
        XDG_CONFIG_HOME=str(fixture / "ime-config"),
        XDG_CACHE_HOME=str(fixture / "ime-cache"),
        XDG_DATA_HOME=str(fixture / "ime-data"),
        GSETTINGS_SCHEMA_DIR="/usr/share/glib-2.0/schemas",
        GSETTINGS_BACKEND="keyfile",
    )
    command("gsettings", "set", "org.freedesktop.ibus.panel", "lookup-table-orientation", "1")
    module_cache = fixture / "gtk-immodules.cache"
    module_cache.write_text(command("gtk-query-immodules-3.0", os.environ["SHEPHERD_SMOKE_IBUS_MODULE"]))

    with subprocess.Popen(["ibus-daemon", "--address", os.environ["IBUS_ADDRESS"],
                           "--panel", "/usr/libexec/ibus-ui-gtk3"]) as daemon:
        try:
            wait_for("IBus session socket", lambda: address if address.is_socket() else None)
            IBus.init()
            bus = IBus.Bus.new()
            wait_for("IBus bus connection", lambda: bus if bus.is_connected() else None)
            # IBus exposes a placeholder context before the GTK client takes focus.
            idle_context = bus.current_input_context()
            log = fixture / "linux-ime.log"
            with log.open("w") as output, subprocess.Popen(
                ["timeout", "--signal=INT", "--kill-after=10s", "120s", *launch],
                env={**app_environment, "GDK_BACKEND": "x11", "GTK_IM_MODULE": "ibus",
                     "GTK_IM_MODULE_FILE": str(module_cache)},
                stdout=output, stderr=subprocess.STDOUT,
            ) as app:
                try:
                    check_composition(log, app, bus, idle_context)
                    if app.wait(timeout=15):
                        raise RuntimeError(f"The IME smoke app exited with status {app.returncode}")
                finally:
                    if app.poll() is None:
                        app.send_signal(signal.SIGINT)
                        app.wait()
                    print(log.read_text(), end="", flush=True)
        finally:
            daemon.terminate()


if __name__ == "__main__":
    main(Path(sys.argv[1]), sys.argv[2:])
