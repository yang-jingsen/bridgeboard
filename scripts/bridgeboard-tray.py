#!/usr/bin/env python3
"""Small Qt tray frontend for the bridgeboard CLI."""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import threading
import webbrowser
from pathlib import Path

from PyQt6.QtCore import QObject, QTimer, pyqtSignal
from PyQt6.QtGui import QAction, QColor, QIcon, QPixmap
from PyQt6.QtWidgets import QApplication, QMenu, QSystemTrayIcon


ROOT = Path(__file__).resolve().parents[1]


def bridgeboard_bin() -> str:
    env = os.environ.get("BRIDGEBOARD_BIN")
    if env:
        return env
    path = shutil.which("bridgeboard")
    if path:
        return path
    for candidate in [
        ROOT / "target" / "release" / exe_name("bridgeboard"),
        ROOT / "target" / "debug" / exe_name("bridgeboard"),
    ]:
        if candidate.exists():
            return str(candidate)
    return "bridgeboard"


def bridgeboard_ui_bin() -> str | None:
    env = os.environ.get("BRIDGEBOARD_UI_BIN")
    if env:
        return env
    path = shutil.which("bridgeboard-ui")
    if path:
        return path
    for candidate in [
        ROOT / "target" / "release" / exe_name("bridgeboard-ui"),
        ROOT / "target" / "debug" / exe_name("bridgeboard-ui"),
    ]:
        if candidate.exists():
            return str(candidate)
    return None


def exe_name(name: str) -> str:
    return f"{name}.exe" if sys.platform.startswith("win") else name


def startupinfo():
    if not sys.platform.startswith("win"):
        return None
    info = subprocess.STARTUPINFO()
    info.dwFlags |= subprocess.STARTF_USESHOWWINDOW
    return info


def run_bridge(args: list[str], timeout: int = 20) -> tuple[int, str, str]:
    try:
        proc = subprocess.run(
            [bridgeboard_bin(), *args],
            text=True,
            capture_output=True,
            timeout=timeout,
            startupinfo=startupinfo(),
        )
        return proc.returncode, proc.stdout.strip(), proc.stderr.strip()
    except Exception as exc:  # Keep tray alive even if CLI is missing.
        return 1, "", str(exc)


def local_machine_name() -> str:
    return os.environ.get("BRIDGEBOARD_MACHINE_ID", socket.gethostname()).lower()


def open_terminal(args: list[str]) -> bool:
    cmd = [bridgeboard_bin(), *args]
    if sys.platform.startswith("win"):
        subprocess.Popen(["cmd", "/c", "start", "Bridgeboard", *cmd])
        return True

    terminal_specs = [
        ("konsole", ["konsole", "--noclose", "-e", *cmd]),
        ("x-terminal-emulator", ["x-terminal-emulator", "-e", *cmd]),
        ("gnome-terminal", ["gnome-terminal", "--", *cmd]),
        ("xfce4-terminal", ["xfce4-terminal", "-e", " ".join(cmd)]),
    ]
    for executable, command in terminal_specs:
        if shutil.which(executable):
            subprocess.Popen(command)
            return True
    return False


def open_bridgeboard_ui() -> bool:
    path = bridgeboard_ui_bin()
    if not path:
        return False
    subprocess.Popen(
        [path],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return True


class BridgeTray(QObject):
    action_done = pyqtSignal(str, int, str, str)
    status_done = pyqtSignal(int, str, str)

    def __init__(self, app: QApplication) -> None:
        super().__init__()
        self.app = app
        self.services: list[dict] = []
        self.refresh_running = False

        self.tray = QSystemTrayIcon(self.icon(), app)
        self.menu = QMenu()
        self.tray.setContextMenu(self.menu)
        self.tray.setToolTip("Bridgeboard")
        self.tray.activated.connect(self.on_activated)

        self.action_done.connect(self.on_action_done)
        self.status_done.connect(self.on_status_done)

        self.timer = QTimer(self)
        self.timer.timeout.connect(self.refresh)
        self.timer.start(5000)

        self.rebuild_menu("loading")
        self.tray.show()
        self.run_async("Startup", ["startup"], 60)
        self.refresh()

    def icon(self) -> QIcon:
        icon = QIcon.fromTheme("network-server")
        if not icon.isNull():
            return icon
        pixmap = QPixmap(32, 32)
        pixmap.fill(QColor("#2563eb"))
        return QIcon(pixmap)

    def on_activated(self, reason: QSystemTrayIcon.ActivationReason) -> None:
        if reason == QSystemTrayIcon.ActivationReason.Trigger:
            self.refresh()
            self.menu.popup(self.tray.geometry().center())

    def refresh(self) -> None:
        if self.refresh_running:
            return
        self.refresh_running = True

        def worker() -> None:
            code, out, err = run_bridge(["ports", "--json", "--peers"], timeout=15)
            self.status_done.emit(code, out, err)

        threading.Thread(target=worker, daemon=True).start()

    def on_status_done(self, code: int, out: str, err: str) -> None:
        self.refresh_running = False
        if code == 0:
            try:
                self.services = json.loads(out or "[]")
                self.rebuild_menu(None)
                return
            except json.JSONDecodeError as exc:
                err = str(exc)
        self.rebuild_menu(err or "status failed")

    def rebuild_menu(self, error: str | None) -> None:
        self.menu.clear()
        summary = f"{len(self.services)} service(s)"
        if error:
            summary = f"Bridgeboard: {error}"
        header = QAction(summary, self.menu)
        header.setEnabled(False)
        self.menu.addAction(header)
        self.menu.addSeparator()

        refresh = QAction("Refresh", self.menu)
        refresh.triggered.connect(self.refresh)
        self.menu.addAction(refresh)
        open_ui = QAction("Open Bridgeboard", self.menu)
        open_ui.triggered.connect(self.open_ui)
        self.menu.addAction(open_ui)

        self.menu.addSeparator()
        if not self.services and not error:
            empty = QAction("No registered services", self.menu)
            empty.setEnabled(False)
            self.menu.addAction(empty)

        for service in self.services:
            service_id = str(service.get("id", "unknown"))
            port = service.get("port", "?")
            mode = service.get("service_mode", service.get("role", "?"))
            state = service.get("runtime_status", service.get("service", "?"))
            owner = str(service.get("owner_host", "?"))
            source = str(service.get("source_machine", ""))
            tunnel_modes = str(service.get("tunnel_modes", ""))
            network_url = service.get("network_url")
            is_remote = bool(source and source.lower() != local_machine_name())
            item = self.menu.addMenu(f"{service_id} :{port} [{mode}/{state}]")
            item.addAction(self.action("Open", ["open", service_id]))
            if is_remote and network_url:
                item.addAction(self.url_action("Open Network URL", str(network_url)))
            if is_remote:
                item.addAction(self.action(f"Start on {owner}", ["remote-up", service_id], timeout=120))
                if "local" in tunnel_modes:
                    item.addAction(self.action("Open via Tunnel", ["open", service_id], timeout=60))
                item.addAction(self.terminal_action("Status", ["status", service_id, "--peers"]))
            else:
                item.addAction(self.action("Up", ["up", service_id], timeout=60))
                item.addAction(self.action("Down", ["down", service_id], timeout=30))
                item.addAction(self.action("Restart", ["restart", service_id], timeout=90))
            logs = QAction("Logs", self.menu)
            logs.triggered.connect(lambda _=False, sid=service_id: self.terminal(["logs", sid, "--lines", "160"]))
            item.addAction(logs)

        self.menu.addSeparator()
        self.menu.addAction(self.terminal_action("Doctor", ["doctor"]))
        self.menu.addAction(self.terminal_action("Port Plan", ["port-plan"]))
        self.menu.addSeparator()
        quit_action = QAction("Quit", self.menu)
        quit_action.triggered.connect(self.app.quit)
        self.menu.addAction(quit_action)
        self.tray.setToolTip(f"Bridgeboard - {summary}")

    def action(self, label: str, args: list[str], timeout: int = 20) -> QAction:
        action = QAction(label, self.menu)
        action.triggered.connect(lambda _=False: self.run_async(label, args, timeout))
        return action

    def terminal_action(self, label: str, args: list[str]) -> QAction:
        action = QAction(label, self.menu)
        action.triggered.connect(lambda _=False: self.terminal(args))
        return action

    def url_action(self, label: str, url: str) -> QAction:
        action = QAction(label, self.menu)
        action.triggered.connect(lambda _=False: webbrowser.open(url))
        return action

    def terminal(self, args: list[str]) -> None:
        if not open_terminal(args):
            self.run_async(" ".join(args), args, 30)

    def open_ui(self) -> None:
        if not open_bridgeboard_ui():
            self.terminal(["dashboard"])

    def run_async(self, label: str, args: list[str], timeout: int) -> None:
        def worker() -> None:
            code, out, err = run_bridge(args, timeout=timeout)
            self.action_done.emit(label, code, out, err)

        threading.Thread(target=worker, daemon=True).start()

    def on_action_done(self, label: str, code: int, out: str, err: str) -> None:
        message = out or err or "done"
        if len(message) > 240:
            message = message[:237] + "..."
        icon = (
            QSystemTrayIcon.MessageIcon.Information
            if code == 0
            else QSystemTrayIcon.MessageIcon.Warning
        )
        self.tray.showMessage(f"Bridgeboard: {label}", message, icon, 5000)
        self.refresh()


def main() -> int:
    app = QApplication(sys.argv)
    app.setQuitOnLastWindowClosed(False)
    if not QSystemTrayIcon.isSystemTrayAvailable():
        print("No system tray is available in this session.", file=sys.stderr)
        return 2
    BridgeTray(app)
    return app.exec()


if __name__ == "__main__":
    raise SystemExit(main())
