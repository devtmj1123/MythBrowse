#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 JustMyth / Tan Ming Jing
"""Local server for the WebCrypto check of the Windows smoke test (ci/smoke-windows.ps1).

Serves ci/webcrypto-check.html at http://127.0.0.1:<port>/ (loopback is a secure context, so
crypto.subtle is exposed) and appends the JSON report the page POSTs to /result to --log.

usage: webcrypto_check.py --port 18081 --log <file>
"""
import argparse
import http.server
import os

HERE = os.path.dirname(os.path.abspath(__file__))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=18081)
    ap.add_argument("--log", required=True)
    a = ap.parse_args()
    page = open(os.path.join(HERE, "webcrypto-check.html"), "rb").read()

    class H(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path not in ("/", "/index.html"):
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(page)))
            self.end_headers()
            self.wfile.write(page)

        def do_POST(self):
            n = int(self.headers.get("Content-Length") or 0)
            body = self.rfile.read(min(n, 1 << 20))
            if self.path == "/result":
                with open(a.log, "ab") as f:
                    f.write(body + b"\n")
            self.send_response(204)
            self.end_headers()

        def log_message(self, fmt, *args):
            with open(a.log + ".access", "a") as f:
                f.write((fmt % args) + "\n")

    http.server.ThreadingHTTPServer(("127.0.0.1", a.port), H).serve_forever()


if __name__ == "__main__":
    main()
