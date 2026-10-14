#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 JustMyth / Tan Ming Jing
"""OpenAI-compatible mock model for the CI smoke test of the open build (no real model).

  python3 ci/mock_chat.py --port 18080 --key sk-smoke-... --log mock-chat.log

* GET  /v1/models            -> {"data": [{"id": "mock-myth-1"}]}
* POST /v1/chat/completions  -> a short streamed answer (server-sent events, like OpenAI).
  The open build never sends tool definitions; the log records how many arrived.
Requests without "Authorization: Bearer <key>" get 401 (with the key echoed back redacted, the
way real servers do, so the client's redaction is exercised). The log never contains the key.
"""
import argparse
import json
import re
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ARGS = None
MODEL = "mock-myth-1"


def log(line):
    with open(ARGS.log, "a", encoding="utf-8") as f:
        f.write(time.strftime("%H:%M:%S ") + line + "\n")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *a):  # quiet stderr
        pass

    def auth_ok(self):
        return self.headers.get("Authorization", "") == "Bearer " + ARGS.key

    def send_json(self, status, obj):
        body = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path.rstrip("/").endswith("/models"):
            log(f"GET {self.path} auth={'ok' if self.auth_ok() else 'BAD'}")
            if not self.auth_ok():
                return self.send_json(401, {"error": {"message": "bad key"}})
            return self.send_json(200, {"object": "list", "data": [{"id": MODEL, "object": "model"}]})
        self.send_json(404, {"error": {"message": "not found"}})

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length)
        if not self.path.rstrip("/").endswith("/chat/completions"):
            return self.send_json(404, {"error": {"message": "not found"}})
        if not self.auth_ok():
            log(f"POST {self.path} auth=BAD")
            got = self.headers.get("Authorization", "")
            return self.send_json(401, {"error": {"message": "Incorrect API key provided: " + got[7:11] + "****"}})
        try:
            req = json.loads(raw)
        except ValueError:
            return self.send_json(400, {"error": {"message": "bad json"}})
        msgs = req.get("messages", [])
        users = [m.get("content", "") for m in msgs if m.get("role") == "user"]
        user = users[-1] if users else ""
        log(f"POST chat auth=ok stream={req.get('stream')} tools={len(req.get('tools') or [])} "
            f"messages={len(msgs)} user={user[:60]!r}")
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        reply = ("Hello! I'm Myth. This reply is streamed from the CI mock model, so the chat, "
                 "streaming and the API key path are exercised end to end.")
        for part in re.findall(r"\S+\s*", reply):
            chunk = {"id": "mock", "object": "chat.completion.chunk", "model": MODEL,
                     "choices": [{"index": 0, "delta": {"content": part}, "finish_reason": None}]}
            self.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
            self.wfile.flush()
            time.sleep(0.03)
        end = {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}
        self.wfile.write(("data: " + json.dumps(end) + "\n\n").encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()
        self.close_connection = True


def main():
    global ARGS
    p = argparse.ArgumentParser()
    p.add_argument("--port", type=int, default=18080)
    p.add_argument("--key", required=True)
    p.add_argument("--log", default="mock-chat.log")
    ARGS = p.parse_args()
    log(f"mock chat model on 127.0.0.1:{ARGS.port}")
    ThreadingHTTPServer(("127.0.0.1", ARGS.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
