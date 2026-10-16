#!/usr/bin/env python3
"""公開 REST API と OPC UA クライアントから Hub の listener を検証する。"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

from asyncua import Client, ua


class Rest:
    def __init__(self, base: str):
        self.base = base
        self.token: str | None = None
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def call(self, method: str, path: str, body=None, *, admin=True, key=None, timeout=10):
        headers = {"Accept": "application/json", "X-Banto-Client": "banto"}
        if body is not None:
            headers["Content-Type"] = "application/json"
        if admin and self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        if key:
            headers["Authorization"] = f"Bearer {key}"
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(self.base + path, data=data, headers=headers, method=method)
        try:
            with self.opener.open(request, timeout=timeout) as response:
                raw = response.read()
                return response.status, json.loads(raw) if raw else {}
        except urllib.error.HTTPError as exc:
            raw = exc.read()
            try:
                detail = json.loads(raw)
            except ValueError:
                detail = raw.decode(errors="replace")
            raise RuntimeError(f"REST {method} {path} returned {exc.code}: {detail}") from exc

    def expect(self, method, path, body=None, *, admin=True, key=None, timeout=10):
        status, value = self.call(method, path, body, admin=admin, key=key, timeout=timeout)
        if not 200 <= status < 300:
            raise RuntimeError(f"REST {method} {path} returned {status}: {value}")
        return value


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def child_env(root: Path, http_port: int, profile: str) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("BANTO_") and k != "PORT"}
    config = root / "profiles" / profile / "config"
    data = root / "profiles" / profile / "data"
    config.mkdir(parents=True, exist_ok=True)
    data.mkdir(parents=True, exist_ok=True)
    env.update(
        BANTO_HUB_ROOT=str(root),
        BANTO_HUB_PROFILE=profile,
        BANTO_DB=str(config / "hub.sqlite3"),
        BANTO_HUB_DATA=str(data),
        BANTO_BIND="127.0.0.1",
        PORT=str(http_port),
    )
    return env


def start_hub(binary: Path, env: dict[str, str], log):
    return subprocess.Popen(
        [str(binary)], env=env, stdout=log, stderr=subprocess.STDOUT,
        stdin=subprocess.DEVNULL,
    )


def wait_rest(rest: Rest, process: subprocess.Popen, timeout: float = 60):
    deadline = time.monotonic() + timeout
    last_error = "not ready"
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"Hub exited during startup with status {process.returncode}")
        try:
            rest.expect("GET", "/api/v1/openapi.json", admin=False)
            return
        except Exception as exc:  # deadline 内で REST の起動を待つ。
            last_error = str(exc)
            time.sleep(0.2)
    raise TimeoutError(f"Hub REST did not become ready: {last_error}")


def stop_hub(process: subprocess.Popen, *, cleanup=False):
    if process.poll() is not None:
        if not cleanup and process.returncode != 0:
            raise RuntimeError(f"Hub exited unexpectedly: {process.returncode}")
        return
    process.send_signal(2)  # SIGINT で console の正常終了経路へ入る。
    try:
        code = process.wait(timeout=20)
    except subprocess.TimeoutExpired:
        if not cleanup:
            raise TimeoutError("Hub did not stop cleanly after SIGINT")
        process.kill()
        process.wait(timeout=10)
        return
    if not cleanup and code != 0:
        raise RuntimeError(f"Hub did not exit successfully after SIGINT: {code}")


def wait_opcua_running(rest: Rest, timeout: float = 60):
    """PUT /api/opcua-settings は起動の完了を待たずに返る（#634）。状態の API で起動済みを待つ。"""
    deadline = time.monotonic() + timeout
    status = None
    while time.monotonic() < deadline:
        status = rest.expect("GET", "/api/opcua-status")
        if status["state"] == "running":
            return status
        if status["state"] == "failed":
            raise RuntimeError(f"OPC UA listener failed to start: {status}")
        time.sleep(0.2)
    raise TimeoutError(f"OPC UA listener did not start: {status}")


async def wait_endpoint(url: str, timeout=60):
    deadline = time.monotonic() + timeout
    last_error = "no connection attempt"
    while time.monotonic() < deadline:
        client = Client(url, timeout=3)
        try:
            await client.connect()
            await client.disconnect()
            return
        except Exception as exc:
            last_error = str(exc)
            try:
                await client.disconnect()
            except Exception:
                pass
            await asyncio.sleep(0.2)
    raise TimeoutError(f"OPC UA endpoint did not become ready: {last_error}")


async def node_for_tag(client: Client, external_name: str):
    ns = await client.get_namespace_index("urn:banto:opcua:tags")
    node = client.get_node(ua.NodeId(external_name, ns))
    await node.read_browse_name()
    return ns, node


async def run(binary: Path):
    http_port = free_port()
    opcua_port = free_port()
    while opcua_port == http_port:
        opcua_port = free_port()
    profile = "interop-201"
    with tempfile.TemporaryDirectory(prefix="banto-hub-opcua-") as temp:
        root = Path(temp)
        env = child_env(root, http_port, profile)
        env["BANTO_ALLOW_SETUP"] = "1"
        log_path = root / "hub.log"
        process = None
        api_key = None
        with log_path.open("w+b") as log:
            rest = Rest(f"http://127.0.0.1:{http_port}")
            try:
                process = start_hub(binary, env, log)
                wait_rest(rest, process)
                admin_password = "Interop-201-test-password!"
                session = rest.expect("POST", "/api/auth/setup", {
                    "username": "interop-admin", "password": admin_password,
                    "displayName": "Interop test",
                }, admin=False)
                rest.token = session["token"]

                connections = rest.expect("GET", "/api/plc-connections")
                mem = next((c for c in connections if c.get("name") == "mem"), None)
                if not mem:
                    raise RuntimeError("reserved mem connection was not present")
                group = rest.expect("POST", "/api/collection-groups", {
                    "name": "interop", "plcConnectionId": mem["id"], "periodMs": 1000,
                    "enabled": True, "defaultWritable": True,
                })
                tag = rest.expect("POST", "/api/tags", {
                    "name": "number", "collectionGroupId": group["id"], "address": "",
                    "dataType": "f32", "enabled": True, "writable": True,
                    "tagKind": "internal", "retain": False,
                })
                external_name = "mem.interop.number"
                rest.expect("POST", "/api/collection/start", {})
                issued = rest.expect("POST", "/api/api-keys", {
                    "name": "interop smoke key", "scopes": [f"write:{external_name}"],
                })
                api_key = issued["key"]
                rest.expect("POST", f"/api/v1/values/{external_name}", {"v": 12.5}, admin=False, key=api_key)
                rest.expect("PUT", "/api/opcua-settings", {
                    "enabled": True, "bind": "127.0.0.1", "port": opcua_port,
                    "endpointPath": "/", "namespaceUri": "urn:banto:opcua:tags",
                    "advertisedHost": None,
                })
                endpoint = f"opc.tcp://127.0.0.1:{opcua_port}/"
                running = wait_opcua_running(rest)
                if running["endpoint"] != endpoint:
                    raise RuntimeError(f"OPC UA status reports endpoint {running['endpoint']!r}, expected {endpoint!r}")
                await wait_endpoint(endpoint)

                async with Client(endpoint) as client:
                    endpoints = await client.get_endpoints()
                    assert any(e.EndpointUrl == endpoint for e in endpoints), endpoints
                    objects = client.nodes.objects
                    browse_names = [str((await child.read_browse_name()).Name) for child in await objects.get_children()]
                    assert "Banto" in browse_names, browse_names
                    namespace = await client.get_namespace_index("urn:banto:opcua:tags")
                    browsed = await objects.get_child(
                        [f"{namespace}:Banto", f"{namespace}:mem",
                         f"{namespace}:interop", f"{namespace}:number"]
                    )
                    ns, node = await node_for_tag(client, external_name)
                    assert browsed.nodeid == node.nodeid
                    initial_deadline = time.monotonic() + 10
                    while True:
                        initial = await node.read_data_value(raise_on_bad_status=False)
                        if initial.StatusCode.is_good() and initial.Value.Value == 12.5:
                            break
                        if time.monotonic() >= initial_deadline:
                            raise TimeoutError(f"initial tag value did not become Good/12.5: {initial}")
                        await asyncio.sleep(0.1)
                    assert initial.Value.VariantType == ua.VariantType.Double, initial.Value.VariantType

                    queue: asyncio.Queue = asyncio.Queue()

                    class Handler:
                        def datachange_notification(self, changed_node, value, data):
                            queue.put_nowait((changed_node, value, data))

                    subscription = await client.create_subscription(100, Handler())
                    handle = await subscription.subscribe_data_change(node)
                    rest.expect("POST", f"/api/v1/values/{external_name}", {"v": 42.5}, admin=False, key=api_key)
                    deadline = time.monotonic() + 15
                    while True:
                        remaining = deadline - time.monotonic()
                        if remaining <= 0:
                            raise TimeoutError("subscription did not report the REST-written value")
                        changed_node, value, data = await asyncio.wait_for(queue.get(), timeout=remaining)
                        if value == 42.5:
                            assert data.monitored_item.Value.StatusCode.is_good(), data.monitored_item.Value.StatusCode
                            break
                    first = await node.read_data_value()
                    second = await node.read_data_value()
                    assert first.StatusCode.is_good() and second.StatusCode.is_good()
                    assert first.Value.Value == second.Value.Value == 42.5
                    assert first.SourceTimestamp is not None and first.SourceTimestamp == second.SourceTimestamp
                    assert data.monitored_item.Value.SourceTimestamp == first.SourceTimestamp
                    try:
                        await node.write_value(99.0)
                    except ua.UaStatusCodeError as exc:
                        assert exc.code in (ua.StatusCodes.BadNotWritable, ua.StatusCodes.BadUserAccessDenied), exc
                    else:
                        raise AssertionError("OPC UA Write unexpectedly succeeded")
                    after = await node.read_data_value()
                    assert after.Value.Value == 42.5 and after.StatusCode.is_good()
                    await subscription.unsubscribe(handle)
                    await subscription.delete()
                    node_key = (ns, node.nodeid.Identifier)

                stop_hub(process)
                process = None
                env.pop("BANTO_ALLOW_SETUP", None)
                process = start_hub(binary, env, log)
                rest = Rest(f"http://127.0.0.1:{http_port}")
                wait_rest(rest, process)
                rest.token = rest.expect("POST", "/api/auth/login", {
                    "username": "interop-admin", "password": admin_password,
                }, admin=False)["token"]
                settings = rest.expect("GET", "/api/opcua-settings")
                assert settings["enabled"] and settings["port"] == opcua_port
                rest.expect("POST", "/api/collection/start", {})
                rest.expect("POST", f"/api/v1/values/{external_name}", {"v": 42.5}, admin=False, key=api_key)
                await wait_endpoint(endpoint)
                async with Client(endpoint) as client:
                    ns, node = await node_for_tag(client, external_name)
                    assert (ns, node.nodeid.Identifier) == node_key
                    deadline = time.monotonic() + 10
                    while True:
                        status = await node.read_data_value(raise_on_bad_status=False)
                        if status.StatusCode.is_good() and status.Value.Value == 42.5:
                            break
                        if time.monotonic() >= deadline:
                            raise TimeoutError(f"restarted tag did not become Good/42.5: {status}")
                        await asyncio.sleep(0.1)
                stop_hub(process)
                process = None
                print("OPC UA interop smoke passed (GetEndpoints, Browse, Read, subscription, REST write, UA write rejection, restart persistence).")
            except BaseException:
                if process is not None:
                    stop_hub(process, cleanup=True)
                    process = None
                log.flush()
                log.seek(max(0, log.tell() - 8192))
                tail = log.read().decode(errors="replace")
                if api_key:
                    tail = tail.replace(api_key, "<redacted-api-key>")
                tail = tail.replace("Interop-201-test-password!", "<redacted-password>")
                print("Hub log tail (failure context):\n" + "\n".join(tail.splitlines()[-40:]), file=sys.stderr)
                raise
            finally:
                if process is not None:
                    stop_hub(process, cleanup=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("hub_binary", type=Path, help="path to an already-built banto-hub executable")
    args = parser.parse_args()
    binary = args.hub_binary.resolve(strict=True)
    if not binary.is_file():
        parser.error("hub_binary must be a file")
    asyncio.run(asyncio.wait_for(run(binary), timeout=300))


if __name__ == "__main__":
    main()
