#!/usr/bin/env python3
"""Exercise the shipped proxy with real Nginx and an isolated HTTP upstream."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen
import json
import shutil
import socket
import subprocess
import tempfile
import threading
import time


class Upstream(BaseHTTPRequestHandler):
    def respond(self):
        self.server.methods.append(self.command)
        known = self.path in ("/health", "/ready", "/v1/observation")
        status = 200 if known else 404
        if self.path == "/ready" and not self.server.ready:
            status = 503
        body = json.dumps({"ready": self.server.ready}).encode()
        self.send_response(status)
        # Match the service's header contract; the edge must not duplicate it.
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    do_GET = respond
    do_HEAD = respond
    do_POST = respond

    def log_message(self, *_):
        pass


def main():
    nginx = shutil.which("nginx")
    if not nginx and Path("/usr/sbin/nginx").is_file():
        nginx = "/usr/sbin/nginx"
    if not nginx:
        raise SystemExit("Nginx is required for the proxy integration test")
    example = (Path(__file__).resolve().parents[1] / "deploy/nginx-observer.conf").read_text()
    upstream_url = "http://127.0.0.1:8088/"
    assert example.count(upstream_url) == 1

    with ThreadingHTTPServer(("127.0.0.1", 0), Upstream) as upstream:
        upstream.ready = True
        upstream.methods = []
        worker = threading.Thread(target=upstream.serve_forever, daemon=True)
        worker.start()
        try:
            with tempfile.TemporaryDirectory(prefix="observer-proxy-") as temporary:
                root = Path(temporary)
                (root / "logs").mkdir()
                with socket.socket() as reservation:
                    reservation.bind(("127.0.0.1", 0))
                    port = reservation.getsockname()[1]
                # Only the upstream destination changes; header/access rules stay exact.
                proxy = example.replace(upstream_url, f"http://127.0.0.1:{upstream.server_port}/")
                config = root / "nginx.conf"
                config.write_text(
                    f"daemon off; master_process off; pid {root}/nginx.pid;\n"
                    f"error_log {root}/error.log;\nevents {{}}\n"
                    f"http {{ access_log off; server {{ listen 127.0.0.1:{port};\n"
                    + proxy + "\n} }\n"
                )
                command = [nginx, "-p", str(root) + "/", "-c", str(config)]
                subprocess.run(command + ["-t"], check=True, capture_output=True)
                process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                try:
                    def request(path, method="GET"):
                        req = Request(f"http://127.0.0.1:{port}/observer" + path,
                                      method=method, headers={"Origin": "https://workspace.example"})
                        try:
                            response = urlopen(req, timeout=2)
                        except HTTPError as error:
                            response = error
                        with response:
                            for name, value in (("Access-Control-Allow-Origin", "*"),
                                                ("Cache-Control", "no-store"),
                                                ("X-Content-Type-Options", "nosniff")):
                                assert response.headers.get_all(name) == [value], (name, response.headers)
                            return response.status, response.read()

                    deadline = time.monotonic() + 5
                    while True:
                        assert process.poll() is None, (root / "error.log").read_text()
                        try:
                            assert request("/health")[0] == 200
                            break
                        except URLError:
                            if time.monotonic() >= deadline:
                                raise
                            time.sleep(0.05)
                    assert request("/ready")[0] == 200
                    assert request("/v1/observation")[0] == 200
                    assert request("/ready", "HEAD") == (200, b"")
                    upstream.ready = False
                    status, body = request("/ready")
                    assert status == 503 and json.loads(body)["ready"] is False
                    assert request("/ready", "HEAD") == (503, b"")
                    assert request("/missing")[0] == 404
                    assert request("/v1/observation", "POST")[0] == 403
                    assert request("/v1/observation", "OPTIONS")[0] == 403
                    assert set(upstream.methods) == {"GET", "HEAD"}, upstream.methods
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
        finally:
            upstream.shutdown()
            worker.join(timeout=5)
    print("Proxy contract passed: single headers, GET/HEAD, readiness 503, blocked writes")


if __name__ == "__main__":
    main()
