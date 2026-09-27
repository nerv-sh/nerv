"""Shared pty + pyte harness for the ZLE e2e scripts.

One interactive zsh per `Shell`, rendered through pyte so a check reads
the screen the way a user sees it (popup box, grey ghost after the
cursor). Imported by scripts/e2e-zle-*.py that need a rendered screen.
"""
import codecs, fcntl, os, pty, select, signal, struct, subprocess, termios, time

try:
    import pyte
except ImportError:
    print("FAIL — pyte not installed (run with: uv run -q --with pyte python3 ...)")
    raise SystemExit(1)

ROWS, COLS = 30, 120
BOX = set("│╭╰├┤╮╯")
PROMPT = "> "


class Shell:
    """One interactive zsh on a pty, rendered by pyte."""

    def __init__(self, env, home):
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.Stream(self.screen)
        self.dec = codecs.getincrementaldecoder("utf-8")("replace")
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        self.proc = subprocess.Popen(["/bin/zsh"], preexec_fn=os.setsid, cwd=home,
                                     stdin=slave, stdout=slave, stderr=slave,
                                     env=env, close_fds=True)
        os.close(slave)
        self.pump(3.0, until=lambda: self.cursor_line().startswith(PROMPT.rstrip()))

    def pump(self, seconds, until=None):
        deadline = time.time() + seconds
        while time.time() < deadline:
            r, _, _ = select.select([self.master], [], [], 0.05)
            if r:
                try:
                    chunk = os.read(self.master, 65536)
                except OSError:
                    return
                if not chunk:
                    return
                self.stream.feed(self.dec.decode(chunk))
            elif until and until():
                return

    def type(self, text, gap=0.05):
        # One key at a time: the widget runs per keystroke, and a burst
        # lands several keys in one read.
        for ch in text:
            os.write(self.master, ch.encode())
            self.pump(gap)

    def key(self, seq):
        """Send a multi-byte key (an arrow) in one write: split across the
        widget's 10 ms KEYTIMEOUT, zsh reads Esc and `[C` as two keys."""
        os.write(self.master, seq.encode())
        self.pump(0.1)

    def settle(self, pred=None, seconds=4.0):
        self.pump(seconds, until=pred or (lambda: False))
        self.pump(0.3)

    def at_typed(self, typed):
        """The cursor sits right after `typed`: nothing was inserted. The
        line text alone can't tell — a grey ghost may follow the cursor."""
        return self.screen.cursor.x == len(PROMPT + typed)

    def cursor_line(self):
        return self.screen.display[self.screen.cursor.y].rstrip()

    def box(self):
        return [l.rstrip() for l in self.screen.display if any(c in BOX for c in l)]

    def outside_box(self):
        y = self.screen.cursor.y
        return [l.rstrip() for i, l in enumerate(self.screen.display)
                if i != y and not any(c in BOX for c in l)]

    def close(self):
        try:
            os.write(self.master, b"\x03exit\n")
        except OSError:
            pass
        time.sleep(0.2)
        self.proc.send_signal(signal.SIGTERM)
        try:
            self.proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        os.close(self.master)
