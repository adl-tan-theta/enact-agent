#!/usr/bin/env python3
"""Qualify the real native Unix RPC boundary against a local mock provider."""
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parents[2]


class Fixture(BaseHTTPRequestHandler):
    calls = 0
    tools_offered = []
    mcp_methods = []
    missing_mcp_calls = 0
    preexecuted_calls = 0

    def log_message(self, *_args):
        pass

    def do_GET(self):
        if self.path != "/api/tags":
            self.send_error(404)
            return
        encoded = json.dumps({"models": [{"name": "fixture", "model": "fixture"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path == "/mcp-missing":
            type(self).missing_mcp_calls += 1
            self.send_error(503, "Deliberately unavailable local MCP fixture")
            return
        if self.path == "/mcp":
            method = body["method"]
            type(self).mcp_methods.append(method)
            if method == "notifications/initialized":
                self.send_response(202)
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            if method == "initialize":
                result = {"protocolVersion": body["params"]["protocolVersion"],
                          "capabilities": {"tools": {}},
                          "serverInfo": {"name": "local-inventory-fixture", "version": "1"}}
            elif method == "tools/list":
                result = {"tools": [{"name": "echo", "description": "Local inventory proof only",
                                     "inputSchema": {"type": "object", "properties": {}}}]}
            elif method == "tools/call":
                assert body["params"]["name"] == "echo", body
                failed = body["params"].get("arguments", {}).get("fail", False)
                result = {"content": [{"type": "text", "text": (
                    "local MCP execution failed" if failed else "local MCP execution succeeded")}],
                          "isError": failed}
            else:
                raise AssertionError(f"Unexpected MCP operation: {method}")
            encoded = json.dumps({"jsonrpc": "2.0", "id": body["id"], "result": result}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)
            return
        if self.path != "/v1/chat/completions":
            self.send_error(404)
            return
        if body.get("model") == "preexecuted":
            assert body.get("stream") is True, body
            type(self).preexecuted_calls += 1
            packets = [
                {"x_tool_start": {"name": "file_read", "arguments": '{"path":"fixture.txt"}'}},
                {"x_tool_result": {"name": "file_read", "output": "provider preexecuted fixture"}},
                {"choices": [{"index": 0, "delta": {"content": "preexecuted fixture completed"},
                              "finish_reason": "stop"}]},
            ]
            encoded = ("".join("data: " + json.dumps(packet) + "\n\n" for packet in packets)
                       + "data: [DONE]\n\n").encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)
            return
        type(self).calls += 1
        type(self).tools_offered = [t["function"]["name"] for t in body.get("tools", [])]
        message = {"role": "assistant", "content": "native fixture completed"}
        finish = "stop"
        if type(self).calls == 1:
            message = {"role": "assistant", "content": "", "tool_calls": [{"index": 0,
                "id": "fixture-call", "type": "function", "function": {
                "name": "shell", "arguments": json.dumps({"command": "printf enact-probe-must-not-execute"})}}]}
            finish = "tool_calls"
        elif type(self).calls == 2:
            message = {"role": "assistant", "content": "", "tool_calls": [{"index": 0,
                "id": "fixture-mcp-call", "type": "function", "function": {
                "name": "fixture__echo", "arguments": "{}"}}]}
            finish = "tool_calls"
        elif type(self).calls == 3:
            message = {"role": "assistant", "content": "", "tool_calls": [{"index": 0,
                "id": "fixture-mcp-error", "type": "function", "function": {
                "name": "fixture__echo", "arguments": '{"fail":true}'}}]}
            finish = "tool_calls"
        if body.get("stream"):
            response = {"id": "fixture-response", "object": "chat.completion.chunk", "model": "fixture",
                        "choices": [{"index": 0, "delta": message, "finish_reason": finish}]}
            encoded = ("data: " + json.dumps(response) + "\n\ndata: [DONE]\n\n").encode()
            content_type = "text/event-stream"
        else:
            response = {"id": "fixture-response", "object": "chat.completion", "model": "fixture",
                        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
                        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}}
            encoded = json.dumps(response).encode()
            content_type = "application/json"
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    fixture = ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    thread = threading.Thread(target=fixture.serve_forever, daemon=True)
    thread.start()
    process = None
    try:
        with tempfile.TemporaryDirectory(prefix="probe-", dir=ROOT / "target/enact") as name:
            private = Path(name)
            private.chmod(0o700)
            config = private / "config"
            config.mkdir(mode=0o700)
            workspace = private / "workspace"
            workspace.mkdir(mode=0o700)
            endpoint = private / "agent.sock"
            (config / "config.toml").write_text(f'''schema_version = 3
[heartbeat]
enabled = false
[scheduler]
enabled = false
[channels]
cli = false
[providers.models.ollama.fixture]
uri = "http://127.0.0.1:{fixture.server_port}"
model = "fixture"
[providers.models.custom.preexecuted]
uri = "http://127.0.0.1:{fixture.server_port}/v1"
model = "preexecuted"
wire_api = "chat_completions"
[agents.preexecuted]
model_provider = "custom.preexecuted"
risk_profile = "preexecuted"
runtime_profile = "probe"
[agents.preexecuted.workspace]
path = "{workspace}"
[agents.preexecuted.memory]
backend = "none"
[risk_profiles.preexecuted]
level = "supervised"
workspace_only = true
allowed_tools = ["file_read"]
auto_approve = ["file_read"]
[agents.probe]
model_provider = "ollama.fixture"
risk_profile = "probe"
runtime_profile = "probe"
mcp_bundles = ["probe"]
[agents.probe.workspace]
path = "{workspace}"
[agents.probe.memory]
backend = "none"
[risk_profiles.probe]
level = "supervised"
workspace_only = true
allowed_commands = ["printf"]
allowed_tools = ["shell", "fixture__echo", "missing__echo"]
always_ask = ["shell"]
auto_approve = ["fixture__echo"]
[runtime_profiles.probe]
agentic = true
max_tool_iterations = 1
[mcp]
enabled = true
deferred_loading = false
[[mcp.servers]]
name = "fixture"
transport = "http"
url = "http://127.0.0.1:{fixture.server_port}/mcp"
[[mcp.servers]]
name = "missing"
transport = "http"
url = "http://127.0.0.1:{fixture.server_port}/mcp-missing"
[mcp_bundles.probe]
servers = ["fixture", "missing"]
''')
            env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(private),
                   "TMPDIR": str(private), "LANG": "en_US.UTF-8"}
            env.update(ENACT_AGENT_CONFIG_DIR=str(config), ENACT_AGENT_SOCKET=str(endpoint),
                       ENACT_AGENT_runtime_profiles__probe__max_tool_iterations="4")
            with (private / "daemon.log").open("wb") as log:
                process = subprocess.Popen([str(binary), "daemon"],
                                           cwd=workspace, env=env, stdout=log, stderr=log)
                deadline = time.monotonic() + 30
                while not endpoint.exists():
                    if process.poll() is not None:
                        raise RuntimeError(f"daemon exited {process.returncode}: {(private / 'daemon.log').read_text()}")
                    if time.monotonic() > deadline:
                        raise TimeoutError("native socket readiness exceeded 30 seconds")
                    time.sleep(0.05)
                with socket.socket(socket.AF_UNIX) as connection:
                    connection.settimeout(30)
                    connection.connect(str(endpoint))
                    frames = connection.makefile("rb")
                    notifications = []
                    def send(method, params, request_id):
                        connection.sendall((json.dumps({"jsonrpc": "2.0", "method": method,
                            "params": params, "id": request_id}) + "\n").encode())
                    def receive():
                        line = frames.readline()
                        if not line:
                            raise RuntimeError("native socket closed unexpectedly")
                        frame = json.loads(line)
                        if frame.get("jsonrpc") != "2.0":
                            raise AssertionError("invalid JSON-RPC envelope")
                        return frame
                    def request(method, params, request_id):
                        send(method, params, request_id)
                        while True:
                            frame = receive()
                            if frame.get("id") == request_id:
                                if "error" in frame:
                                    raise RuntimeError(f"{method}: {frame['error']}")
                                return frame["result"]
                            notifications.append(frame)
                    initialized = request("initialize", {"protocol_version": 1, "env": {}}, "init")
                    assert initialized["protocol_version"] == 1
                    assert "session/prompt" in initialized["capabilities"]
                    assert "session/tools" in initialized["capabilities"]
                    health = request("health", {}, "health")
                    overridden = request("config/get", {
                        "prop": "runtime_profiles.probe.max_tool_iterations"}, "env-override")
                    assert overridden["value"] == "4", overridden
                    created = request("session/new", {"agent_alias": "probe", "exclude_memory": True}, "new")
                    session = created["session_id"]
                    inventory = request("session/tools", {"session_id": session}, "tools")
                    assert set(inventory) == {"session_id", "tools"}, inventory
                    assert inventory["session_id"] == session, inventory
                    assert "shell" in inventory["tools"], inventory
                    assert "fixture__echo" in inventory["tools"], inventory
                    assert "missing__echo" not in inventory["tools"], inventory
                    assert Fixture.calls == 0, "inventory invoked the model provider"
                    assert Fixture.missing_mcp_calls > 0, "missing MCP fixture was not attempted"
                    assert "tools/list" in Fixture.mcp_methods, Fixture.mcp_methods
                    send("session/prompt", {"session_id": session, "prompt": "Exercise local fixture only.",
                                          "client_turn_generation": 1}, "prompt")
                    approval = None
                    completed = None
                    prompt_result = False
                    approval_acknowledged = False
                    tool_results = []
                    while completed is None or not prompt_result or not approval_acknowledged:
                        frame = receive()
                        if frame.get("id") == "approval":
                            approval_acknowledged = frame.get("result", {}).get("acknowledged") is True
                        if frame.get("id") == "prompt":
                            if "error" in frame:
                                raise RuntimeError(f"prompt failed: {frame['error']}")
                            prompt_result = True
                        if frame.get("method") == "session/update":
                            event = frame["params"]
                            notifications.append(frame)
                            if event.get("type") == "tool_result":
                                assert type(event.get("success")) is bool, event
                                tool_results.append(event)
                            if event.get("type") == "approval_request":
                                approval = event
                                send("session/approve", {"session_id": session,
                                    "request_id": event["request_id"], "decision": "reject"}, "approval")
                            if event.get("type") == "turn_complete":
                                completed = event
                    assert approval_acknowledged, "approval rejection was not acknowledged"
                    assert [(event["name"], event["success"]) for event in tool_results] == [
                        ("shell", False), ("fixture__echo", True), ("fixture__echo", False)], tool_results
                    assert "denied" in tool_results[0]["raw_output"].lower(), tool_results
                    assert approval is not None, "approval event did not cross native RPC"
                    assert completed["outcome"] == "completed", completed
                    assert completed["client_turn_generation"] == 1
                    assert completed["content"] == "native fixture completed", completed
                    assert Fixture.calls == 4, Fixture.calls
                    state = request("session/state", {"session_id": session}, "state")
                    closed = request("session/close", {"session_id": session}, "close")
                    assert closed["closed"] is True
                    send("session/tools", {"session_id": session}, "closed-tools")
                    while True:
                        after_close = receive()
                        if after_close.get("id") == "closed-tools":
                            assert "error" in after_close, after_close
                            break
                    preexecuted = request("session/new", {
                        "agent_alias": "preexecuted", "exclude_memory": True}, "preexecuted-new")
                    preexecuted_session = preexecuted["session_id"]
                    send("session/prompt", {"session_id": preexecuted_session,
                        "prompt": "Exercise only the local provider preexecution fixture.",
                        "client_turn_generation": 1}, "preexecuted-prompt")
                    unknown_result = None
                    preexecuted_complete = None
                    preexecuted_ack = False
                    while preexecuted_complete is None or not preexecuted_ack:
                        frame = receive()
                        if frame.get("id") == "preexecuted-prompt":
                            assert "error" not in frame, frame
                            preexecuted_ack = True
                        if frame.get("method") == "session/update":
                            event = frame["params"]
                            if event.get("type") == "tool_result":
                                assert "success" in event and event["success"] is None, event
                                unknown_result = event
                            if event.get("type") == "turn_complete":
                                preexecuted_complete = event
                    assert unknown_result is not None, "provider preexecuted tool result was not observed"
                    assert preexecuted_complete["outcome"] == "completed", preexecuted_complete
                    assert Fixture.preexecuted_calls == 1, Fixture.preexecuted_calls
                    request("session/close", {"session_id": preexecuted_session}, "preexecuted-close")
                    print(json.dumps({"protocol": "enact-agent-local-rpc", "version": 1,
                        "transport": "unix-ndjson", "auth": "isolated local peer, no credentials",
                        "serverVersion": initialized["server_version"],
                        "checks": ["native ENACT_AGENT_CONFIG_DIR", "native ENACT_AGENT_SOCKET",
                                   "native ENACT_AGENT schema override", "initialize string ID", "health", "session/new", "session/prompt",
                                   "session/tools instantiated MCP inventory", "configured unavailable MCP absent",
                                   "session/tools no provider invocation", "closed session inventory rejected",
                                   "approval_request", "session/approve reject", "turn_complete generation",
                                   "actual rejected tool success false", "actual MCP execution success true",
                                   "actual MCP error success false", "provider preexecution success null",
                                   "session/state", "session/close"],
                        "provider": "local deterministic OpenAI-compatible fixture for Ollama family", "providerCalls": Fixture.calls,
                        "preexecutedProviderCalls": Fixture.preexecuted_calls,
                        "unsupported": ["provider-preexecuted tools with unknown success; Enact must reject null"],
                        "toolsOffered": Fixture.tools_offered,
                        "constructedTools": inventory["tools"],
                        "mcpMethods": Fixture.mcp_methods,
                        "unavailableMcpAttempts": Fixture.missing_mcp_calls,
                        "toolOutcomes": [{"name": event["name"], "success": event["success"]}
                                         for event in tool_results],
                        "socketMode": oct(endpoint.stat().st_mode & 0o777)}, indent=2))
    finally:
        if process and process.poll() is None:
            process.send_signal(signal.SIGTERM)
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        fixture.shutdown()
        fixture.server_close()


if __name__ == "__main__":
    main()
