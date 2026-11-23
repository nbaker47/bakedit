"""Ask the scenario's own herdr server (never the user's) about its panes, over its socket.

    herdr-check.py split           split the focused pane in two, side by side; the new pane
                                   is on the right and the focus stays on the left one
    herdr-check.py focused left    exit 1 unless the focused pane is the left (or right) one
    herdr-check.py workspace NAME focus|keep   create a workspace (screenshots only)
    herdr-check.py zoomed yes|no   exit 1 unless the focused tab is (or is not) zoomed

The socket is $BAKEDIT_SCENARIO_HERDR_SOCKET, which run.sh sets only for the herdr-session
scenario, pointing into the throwaway server's own folder.
"""
import json
import os
import socket
import sys
import time

path = os.environ["BAKEDIT_SCENARIO_HERDR_SOCKET"]
if "/bh." not in path:
    sys.exit(f"refusing a socket that is not a test server's: {path}")


def call(method, params=None):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(3)
        sock.connect(path)
        sock.sendall(json.dumps({"id": "scenario", "method": method, "params": params or {}}).encode() + b"\n")
        reply = b""
        while not reply.endswith(b"\n"):
            chunk = sock.recv(65536)
            if not chunk:
                break
            reply += chunk
    message = json.loads(reply)
    if "error" in message and message["error"]:
        sys.exit(f"{method}: {message['error']}")
    return message.get("result")


def layout():
    return call("pane.layout")["layout"]


def panes_left_to_right():
    return sorted(layout()["panes"], key=lambda pane: (pane["rect"]["x"], pane["rect"]["y"]))


command = sys.argv[1]
if command == "split":
    current = call("pane.current")
    pane = current.get("pane", current)
    call("pane.split", {"target_pane_id": pane["pane_id"], "direction": "right", "ratio": 0.5, "focus": False})
    time.sleep(0.3)
    print(f"split {pane['pane_id']}")
elif command == "workspace":
    result = call("workspace.create", {"label": sys.argv[2], "focus": sys.argv[3] == "focus"})
    time.sleep(0.3)
    print(f"workspace {sys.argv[2]}: {result}")
elif command == "focused":
    want = sys.argv[2]
    panes = panes_left_to_right()
    if len(panes) < 2:
        sys.exit(f"expected two panes, found {len(panes)}")
    index = 0 if want == "left" else len(panes) - 1
    deadline = time.time() + 3
    while not panes[index].get("focused"):
        if time.time() > deadline:
            sys.exit(f"the {want} pane is not focused: {panes}")
        time.sleep(0.1)
        panes = panes_left_to_right()
    print(f"{want} pane focused")
elif command == "zoomed":
    want = sys.argv[2] == "yes"
    deadline = time.time() + 3
    while bool(layout().get("zoomed")) != want:
        if time.time() > deadline:
            sys.exit(f"zoomed is not {want}: {layout()}")
        time.sleep(0.1)
    print(f"zoomed: {want}")
else:
    sys.exit(f"unknown command {command}")
