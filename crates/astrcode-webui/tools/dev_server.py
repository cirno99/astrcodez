#!/usr/bin/env python3
"""Web UI 的开发服务器：静态托管 `www/`，并把 `/api` 反代到 astrcode server。

存在的理由只有一个：让浏览器把 wasm 页面和 `astrcode server` 看成**同源**。
astrcode-server 没有 CORS 层，跨源直连会被浏览器拦掉；同源就没有这个问题，
而这正是「产物由 astrcode-server 内嵌托管」那条路的运行形态。

发 COOP/COEP 头：gpui 的 web 平台依赖 SharedArrayBuffer，而 SharedArrayBuffer
只在 cross-origin isolated 的文档里可用。服务端将来内嵌托管时同样要发这两个头。

用法：python3 tools/dev_server.py [--port 8099] [--api http://127.0.0.1:3847]
"""

import argparse
import http.client
import mimetypes
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

WWW = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "www")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "astrcode-webui-dev"
    api_base = "http://127.0.0.1:3847"

    def log_message(self, fmt, *args):
        sys.stderr.write("[dev-server] " + (fmt % args) + "\n")

    def _send_common_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cache-Control", "no-store")

    def do_GET(self):
        if urlsplit(self.path).path.startswith("/api/"):
            self._proxy()
        else:
            self._serve_static(urlsplit(self.path).path)

    def do_POST(self):
        if urlsplit(self.path).path.startswith("/api/"):
            self._proxy()
        else:
            self.send_error(405)

    def do_PUT(self):
        self.do_POST()
    def do_PATCH(self):
        self.do_POST()


    def do_DELETE(self):
        self.do_POST()

    def _proxy(self):
        target = urlsplit(self.api_base)
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else None

        connection = http.client.HTTPConnection(target.hostname, target.port or 80, timeout=600)
        try:
            connection.request(self.command, self.path, body=body, headers=self._forward_headers())
            response = connection.getresponse()
        except OSError as error:
            self.send_error(502, f"upstream unreachable: {error}")
            connection.close()
            return

        self.send_response(response.status)
        for name, value in response.getheaders():
            if name.lower() in {"transfer-encoding", "connection", "content-length"}:
                continue
            self.send_header(name, value)
        self.send_header("Connection", "close")
        self._send_common_headers()
        self.end_headers()

        try:
            while True:
                # `read(n)` 会读满 n 字节才返回，对 SSE 等于整包缓冲；
                # `read1(n)` 有多少给多少，才能逐块透传。
                chunk = response.read1(4096)
                if not chunk:
                    break
                self.wfile.write(chunk)
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            connection.close()
            self.close_connection = True

    def _forward_headers(self):
        skip = {"host", "connection", "accept-encoding", "transfer-encoding", "content-length"}
        return {k: v for k, v in self.headers.items() if k.lower() not in skip}

    def _serve_static(self, path):
        if path == "/":
            path = "/index.html"
        relative = os.path.normpath(path.lstrip("/"))
        if relative.startswith(".."):
            self.send_error(403)
            return
        full = os.path.join(WWW, relative)
        if not os.path.isfile(full):
            self.send_error(404, f"no such file: {relative}")
            return

        with open(full, "rb") as handle:
            content = handle.read()
        content_type, _ = mimetypes.guess_type(full)
        if full.endswith(".wasm"):
            content_type = "application/wasm"
        elif full.endswith(".js"):
            content_type = "text/javascript"
        self.send_response(200)
        self.send_header("Content-Type", content_type or "application/octet-stream")
        self.send_header("Content-Length", str(len(content)))
        self._send_common_headers()
        self.end_headers()
        self.wfile.write(content)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8099)
    parser.add_argument("--api", default="http://127.0.0.1:3847")
    args = parser.parse_args()

    Handler.api_base = args.api

    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.daemon_threads = True
    print(
        f"dev server: http://127.0.0.1:{args.port}  (api -> {args.api})",
        flush=True,
    )
    threading.current_thread().name = "dev-server"
    server.serve_forever()


if __name__ == "__main__":
    main()
