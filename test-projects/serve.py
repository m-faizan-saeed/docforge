"""Serves the repository for testing the web build (target/web).

    python3 test-projects/serve.py [port]

Then open http://localhost:8000/target/web/index.html, or with a project:
http://localhost:8000/target/web/index.html?open=../../test-projects/pcb-overlay.json

`/__delay?ms=N` answers after N ms with a 1-pixel image; a page that shows it
fires its load event only then (used by web-screenshot.sh).
"""
import http.server
import os
import sys
import time
from urllib.parse import parse_qs, urlparse

PIXEL = bytes.fromhex(
    "47494638396101000100800000000000ffffff21f90401000000002c00000000010001000002024401003b"
)


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map, ".wasm": "application/wasm"}

    def do_GET(self):
        url = urlparse(self.path)
        if url.path == "/__delay":
            time.sleep(int(parse_qs(url.query).get("ms", ["0"])[0]) / 1000)
            self.send_response(200)
            self.send_header("Content-Type", "image/gif")
            self.send_header("Content-Length", str(len(PIXEL)))
            self.end_headers()
            self.wfile.write(PIXEL)
            return
        super().do_GET()

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    os.chdir(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
    http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
