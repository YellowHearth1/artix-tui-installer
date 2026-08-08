#!/usr/bin/env python3
"""Talk to a running QEMU over QMP: press keys, photograph the screen, quit.

Driven by `scripts/qemu-test.sh` — see its `drive`, `key`, `type`, `shot` and
`stop` modes. Not meant to be run by hand.

WHY THIS EXISTS. The installer's own tests prove that a screen does not panic
and that a plan comes out right. They cannot tell whether the thing on screen
makes sense: whether a hint names the key it should, whether a choice made on
step 2 is still there on step 11, whether a list opens where it ought to. That
needs somebody to walk the wizard — and walking it by hand, on a rebuild, for
every change, is exactly the work nobody does twice.

So: boot the image headless, send keystrokes, take pictures. The pictures are
what gets looked at.

Keys are QEMU qcode names (`ret`, `down`, `esc`, `spc`, `a`, `1`, …), because
that is what `send-key` speaks. `type` spells a word out into them so a search
filter can be typed without naming every letter.
"""

import json
import socket
import sys
import time

# The few names that are not simply the character. Everything else — letters,
# digits — is its own qcode.
SPECIAL = {
    " ": "spc",
    "-": "minus",
    "_": "shift-minus",
    "/": "slash",
    ".": "dot",
    ",": "comma",
    "+": "shift-equal",
    "=": "equal",
}


class Qmp:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.f = self.sock.makefile("rw", buffering=1, encoding="utf-8", newline="\n")
        self.f.readline()  # greeting
        self.cmd("qmp_capabilities")

    def cmd(self, name, **args):
        self.f.write(json.dumps({"execute": name, "arguments": args}) + "\n")
        while True:
            # Events interleave with replies; only a reply ends the wait.
            reply = json.loads(self.f.readline())
            if "return" in reply or "error" in reply:
                return reply

    def key(self, name):
        """One keystroke. `shift-x` presses both together, which is how a
        capital letter or a symbol on the top row has to be sent."""
        parts = name.split("-")
        keys = [{"type": "qcode", "data": p} for p in parts]
        r = self.cmd("send-key", keys=keys)
        if "error" in r:
            raise SystemExit(f"send-key {name}: {r['error']['desc']}")
        # A real keyboard cannot outrun the guest's input layer; a script can.
        # Without a pause between keys the installer misses some of them, and
        # the screenshot then shows a state nobody asked for.
        time.sleep(0.12)

    def text(self, s):
        for ch in s:
            if ch in SPECIAL:
                self.key(SPECIAL[ch])
            elif ch.isupper():
                self.key(f"shift-{ch.lower()}")
            else:
                self.key(ch)

    def shot(self, path):
        r = self.cmd("screendump", filename=path, format="png")
        if "error" in r:
            r = self.cmd("screendump", filename=path)
            if "error" in r:
                raise SystemExit("screendump: " + r["error"]["desc"])


def main():
    if len(sys.argv) < 3:
        raise SystemExit("usage: qmp.py <socket> <key|type|shot|quit> [args...]")
    sock, verb, rest = sys.argv[1], sys.argv[2], sys.argv[3:]
    q = Qmp(sock)
    if verb == "key":
        for name in rest:
            q.key(name)
    elif verb == "type":
        q.text(" ".join(rest))
    elif verb == "shot":
        q.shot(rest[0])
    elif verb == "quit":
        q.cmd("quit")
    else:
        raise SystemExit(f"unknown verb: {verb}")


if __name__ == "__main__":
    main()
