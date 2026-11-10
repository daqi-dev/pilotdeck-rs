"""PilotDeck native helpers. Only the server's allowed tools can execute."""
import json as _json
import os as _os
import socket as _socket
import shlex as _shlex
import threading as _threading

_lock = _threading.Lock()
_connection = None
_reader = None

def _call(name, arguments):
    global _connection, _reader
    with _lock:
        if _connection is None:
            _connection = _socket.create_connection(
                ("127.0.0.1", int(_os.environ["PILOTDECK_RPC_PORT"])), timeout=300)
            _reader = _connection.makefile("rb")
        request = {"token": _os.environ["PILOTDECK_RPC_TOKEN"], "tool": name, "args": arguments}
        _connection.sendall((_json.dumps(request) + "\n").encode("utf-8"))
        line = _reader.readline(2 * 1024 * 1024 + 2)
        if not line:
            raise RuntimeError("PilotDeck RPC server disconnected")
        if not line.endswith(b"\n") or len(line) > 2 * 1024 * 1024 + 1:
            raise RuntimeError("PilotDeck RPC response exceeded 2MiB.")
        response = _json.loads(line)
        if response.get("error"):
            raise RuntimeError(response["error"])
        return response

def web_search(query, country=None):
    args = {"query": query}
    if country is not None:
        args["gl"] = country
    return _call("web_search", args)

def web_fetch(url, mode=None, prompt=None):
    args = {"url": url}
    if mode is not None:
        args["mode"] = mode
    if prompt is not None:
        args["prompt"] = prompt
    return _call("web_fetch", args)

def read_file(file_path, offset=0, limit=None):
    args = {"file_path": file_path}
    if offset is not None and offset > 0:
        args["offset"] = offset
    if limit is not None:
        args["limit"] = limit
    return _call("read_file", args)

def write_file(file_path, content):
    return _call("write_file", {"file_path": file_path, "content": content})

def edit_file(file_path, old_string, new_string, replace_all=False):
    return _call("edit_file", {"file_path": file_path, "old_string": old_string,
                              "new_string": new_string, "replace_all": replace_all})

def grep(pattern, path=None, glob=None):
    args = {"pattern": pattern}
    if path is not None:
        args["path"] = path
    if glob is not None:
        args["glob"] = glob
    return _call("grep", args)

def glob(pattern, path=None):
    args = {"pattern": pattern}
    if path is not None:
        args["path"] = path
    return _call("glob", args)

def bash(command, timeout_ms=None, workdir=None):
    args = {"command": command if workdir is None else "cd " + _shlex.quote(workdir) + " && " + command}
    if timeout_ms is not None:
        args["timeout"] = timeout_ms
    return _call("bash", args)

# Export only configured helpers; the RPC allow-list independently enforces it.
if _os.environ.get("PILOTDECK_EXECUTE_CODE_WEB_SEARCH") == "0":
    del web_search
