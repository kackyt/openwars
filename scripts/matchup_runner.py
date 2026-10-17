"""MCPサーバーを使う対局の並列実行・ログ・後始末をまとめた共通部品。

`eval_matchup.py`（1つのバイナリでの評価）と `run_matchup_sweep.py`
（バイナリや環境変数の変種を振る比較）の両方から使う。

- 各ワーカースレッドは自分専用のMCPサーバーを1本持ち、対局を順に処理する。
  サーバーは最初の呼び出し時に起動し、同じ変種の対局で使い回す。
- このプロセスが起動したサーバーだけを登録し、正常終了・例外・Ctrl+Cの
  いずれでも `ServerRegistry.close_all()` で確実に終了させる。他の実行中の
  ベンチのサーバーには触れない。
- Windowsでは起動したサーバーをジョブオブジェクトへ入れ、Pythonが強制終了
  された場合でもOSがサーバーを道連れにして終了させる（孤児プロセスを残さない）。
"""

from __future__ import annotations

import atexit
import json
import os
import subprocess
import sys
import threading
import time
from concurrent.futures import FIRST_COMPLETED, ThreadPoolExecutor, wait
from pathlib import Path
from typing import Any, Callable

REPO_ROOT = Path(__file__).resolve().parent.parent

# 対局結果のうち、進捗表示・軽量な集計に不要な大きい項目。
HEAVY_RESULT_KEYS = frozenset(
    {
        "metrics",
        "final_state",
        "initial_state",
        "invasion_events",
        "transport_history",
        "strategic_history",
        "island_campaign_history",
        "idle_audit_history",
        "operation_assignment_history",
        "production_plan_history",
        "deployment_audit_history",
        "plan_revision_history",
        "plan_execution_history",
        "emergency_plan_history",
        "factory_relief_history",
        "victory_roadmap_history",
        "logistics_plan_history",
        "capital_route_history",
        "action_history",
    }
)


def default_server_path() -> Path:
    """リポジトリの release ビルドの mcp-server を返す。"""
    exe_name = "mcp-server.exe" if os.name == "nt" else "mcp-server"
    return REPO_ROOT / "target" / "release" / exe_name


class _KillOnCloseJob:
    """Windowsのジョブオブジェクト。ハンドルが閉じると所属プロセスを全て終了させる。

    このプロセスが異常終了してハンドルがOSに回収された場合も、サーバーを残さない。
    利用できない環境（非Windows・権限不足）では何もしない。
    """

    def __init__(self) -> None:
        self._handle = None
        if os.name != "nt":
            return
        try:
            import ctypes
            from ctypes import wintypes

            kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

            class IoCounters(ctypes.Structure):
                _fields_ = [
                    (name, ctypes.c_ulonglong)
                    for name in (
                        "ReadOperationCount",
                        "WriteOperationCount",
                        "OtherOperationCount",
                        "ReadTransferCount",
                        "WriteTransferCount",
                        "OtherTransferCount",
                    )
                ]

            class BasicLimit(ctypes.Structure):
                _fields_ = [
                    ("PerProcessUserTimeLimit", ctypes.c_int64),
                    ("PerJobUserTimeLimit", ctypes.c_int64),
                    ("LimitFlags", wintypes.DWORD),
                    ("MinimumWorkingSetSize", ctypes.c_size_t),
                    ("MaximumWorkingSetSize", ctypes.c_size_t),
                    ("ActiveProcessLimit", wintypes.DWORD),
                    ("Affinity", ctypes.c_size_t),
                    ("PriorityClass", wintypes.DWORD),
                    ("SchedulingClass", wintypes.DWORD),
                ]

            class ExtendedLimit(ctypes.Structure):
                _fields_ = [
                    ("BasicLimitInformation", BasicLimit),
                    ("IoInfo", IoCounters),
                    ("ProcessMemoryLimit", ctypes.c_size_t),
                    ("JobMemoryLimit", ctypes.c_size_t),
                    ("PeakProcessMemoryUsed", ctypes.c_size_t),
                    ("PeakJobMemoryUsed", ctypes.c_size_t),
                ]

            # ハンドル値を32bitへ切り詰めないよう、戻り値・引数の型を明示する。
            kernel32.CreateJobObjectW.restype = wintypes.HANDLE
            kernel32.CreateJobObjectW.argtypes = (wintypes.LPVOID, wintypes.LPCWSTR)
            kernel32.SetInformationJobObject.argtypes = (
                wintypes.HANDLE,
                ctypes.c_int,
                wintypes.LPVOID,
                wintypes.DWORD,
            )
            kernel32.AssignProcessToJobObject.argtypes = (wintypes.HANDLE, wintypes.HANDLE)

            handle = kernel32.CreateJobObjectW(None, None)
            if not handle:
                return
            info = ExtendedLimit()
            job_object_limit_kill_on_job_close = 0x2000
            info.BasicLimitInformation.LimitFlags = job_object_limit_kill_on_job_close
            job_object_extended_limit_information = 9
            if not kernel32.SetInformationJobObject(
                handle,
                job_object_extended_limit_information,
                ctypes.byref(info),
                ctypes.sizeof(info),
            ):
                return
            self._kernel32 = kernel32
            self._handle = handle
        except Exception:
            self._handle = None

    def assign(self, process: subprocess.Popen) -> None:
        if self._handle is None:
            return
        try:
            self._kernel32.AssignProcessToJobObject(self._handle, int(process._handle))
        except Exception:
            pass


_JOB = _KillOnCloseJob()


class McpServerClient:
    """MCPサーバー1本とのJSON-RPC接続。最初の呼び出し時に起動する。"""

    def __init__(
        self,
        command: list[str],
        env_overrides: dict[str, str] | None = None,
        stderr_path: Path | None = None,
    ) -> None:
        self.command = command
        self.env_overrides = dict(env_overrides or {})
        self.stderr_path = stderr_path
        self.process: subprocess.Popen | None = None
        self._stderr_file = None
        self._closed = False

    def _ensure_started(self) -> subprocess.Popen:
        if self._closed:
            raise RuntimeError("MCP server client is already closed")
        if self.process is not None:
            return self.process
        env = os.environ.copy()
        env["RUST_LOG"] = "info"
        env.update(self.env_overrides)
        # AIの手順トレースを有効にした場合だけstderrを残す。並列時はサーバーごとの
        # ファイルへ分け、コンソール上で複数サーバーの出力が混ざらないようにする。
        if env.get("OPENWARS_TRACE_AI_STEPS"):
            if self.stderr_path is not None:
                self.stderr_path.parent.mkdir(parents=True, exist_ok=True)
                self._stderr_file = open(self.stderr_path, "w", encoding="utf-8")
                stderr = self._stderr_file
            else:
                stderr = None
        else:
            stderr = subprocess.DEVNULL
        self.process = subprocess.Popen(
            self.command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=stderr,
            text=True,
            encoding="utf-8",
            env=env,
        )
        _JOB.assign(self.process)
        # MCP 初期化シーケンス
        self._send(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "openwars-eval", "version": "0.1.0"},
            },
            req_id=0,
        )
        self._receive()
        return self.process

    def _send(self, method: str, params: dict | None = None, req_id: int = 1) -> None:
        request: dict[str, Any] = {"jsonrpc": "2.0", "id": req_id, "method": method}
        if params:
            request["params"] = params
        process = self.process
        process.stdin.write(json.dumps(request) + "\n")
        process.stdin.flush()

    def _receive(self) -> dict | None:
        line = self.process.stdout.readline()
        if not line:
            return None
        return json.loads(line)

    def call_tool(self, name: str, arguments: dict | None = None, req_id: int = 1):
        """ツールを呼び、結果のJSONを返す（従来の eval_matchup.call_tool と同じ戻り値）。"""
        self._ensure_started()
        self._send("tools/call", {"name": name, "arguments": arguments or {}}, req_id)
        response = self._receive()
        if not response:
            return None
        if "error" in response:
            raise Exception(f"Tool {name} returned error: {response['error']}")
        result_data = response.get("result", {})
        if result_data.get("isError"):
            return {"error": result_data.get("content")}
        content = result_data.get("content", [{"text": "{}"}])[0]["text"]
        try:
            return json.loads(content)
        except json.JSONDecodeError:
            return content

    def close(self) -> None:
        """サーバーを終了させる。何度呼んでもよい。"""
        self._closed = True
        process = self.process
        if process is not None and process.poll() is None:
            try:
                process.stdin.close()
            except Exception:
                pass
            try:
                process.terminate()
                process.wait(timeout=3)
            except Exception:
                try:
                    process.kill()
                    process.wait(timeout=3)
                except Exception:
                    pass
        # 先に終了していたプロセスでもパイプは残るため、両方を必ず閉じる。
        if process is not None:
            for pipe in (process.stdin, process.stdout):
                try:
                    if pipe is not None:
                        pipe.close()
                except Exception:
                    pass
        if self._stderr_file is not None:
            self._stderr_file.close()
            self._stderr_file = None


class ServerRegistry:
    """このプロセスが起動したサーバーの一覧。終了時にすべて後始末する。"""

    def __init__(self) -> None:
        self._clients: list[McpServerClient] = []
        self._lock = threading.Lock()
        atexit.register(self.close_all)

    def create(
        self,
        command: list[str],
        env_overrides: dict[str, str] | None = None,
        stderr_path: Path | None = None,
    ) -> McpServerClient:
        client = McpServerClient(command, env_overrides, stderr_path)
        with self._lock:
            self._clients.append(client)
        return client

    def close_all(self) -> None:
        with self._lock:
            clients, self._clients = self._clients, []
        for client in clients:
            client.close()


class GameLog:
    """1対局分の詳細ログをファイルへ書く。run_single_game の ui_callback に渡す。"""

    def __init__(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        self._file = open(path, "w", encoding="utf-8")

    def __call__(self, event: dict) -> None:
        if event.get("type") == "log":
            self._file.write(f"[LOG] {event['msg']}\n")
        elif event.get("type") == "status_update":
            self._file.write(json.dumps({"type": "metrics", "data": event["metrics"]}) + "\n")
        self._file.flush()

    def close(self) -> None:
        self._file.close()


def slim_result(result: dict) -> dict:
    """進捗表示・集計用に大きい履歴を除いた結果を返す。"""
    return {key: value for key, value in result.items() if key not in HEAVY_RESULT_KEYS}


def run_jobs(
    jobs: list[dict],
    worker_count: int,
    server_key: Callable[[dict], Any],
    make_client: Callable[[dict, int], McpServerClient],
    run_one: Callable[[dict, McpServerClient], dict],
    on_done: Callable[[int, dict, dict, float], None] | None = None,
) -> list[dict]:
    """対局を worker_count 本のサーバーで並列に実行し、入力順の結果リストを返す。

    - `server_key(job)` が同じ対局は同じ設定のサーバーで処理できる。各スレッドは
      直前のサーバーを使い回し、設定が変わったときだけ作り直す。
    - `make_client(job, n)` は n 本目のサーバー接続を作る（起動は最初の呼び出し時）。
    - `on_done(index, job, result, elapsed)` は完了ごとに呼ばれる（呼出しは直列化）。
    戻り値は完了順ではなく `jobs` の並び順。
    """
    results: list[dict | None] = [None] * len(jobs)
    local = threading.local()
    counter_lock = threading.Lock()
    done_lock = threading.Lock()
    server_count = [0]

    def client_for(job: dict) -> McpServerClient:
        key = server_key(job)
        if getattr(local, "key", object()) != key or local.client is None:
            if getattr(local, "client", None) is not None:
                local.client.close()
            with counter_lock:
                server_count[0] += 1
                number = server_count[0]
            local.client = make_client(job, number)
            local.key = key
        return local.client

    def task(index: int) -> tuple[int, dict, float]:
        job = jobs[index]
        started = time.monotonic()
        result = run_one(job, client_for(job))
        return index, result, time.monotonic() - started

    executor = ThreadPoolExecutor(max_workers=max(1, worker_count))
    try:
        pending = {executor.submit(task, index) for index in range(len(jobs))}
        while pending:
            finished, pending = wait(pending, return_when=FIRST_COMPLETED)
            for future in finished:
                index, result, elapsed = future.result()
                results[index] = result
                if on_done is not None:
                    with done_lock:
                        on_done(index, jobs[index], result, elapsed)
    except BaseException:
        # 未着手の対局は取り消す。実行中の対局は呼出し元が close_all() で
        # サーバーを止めることで終了する。
        executor.shutdown(wait=False, cancel_futures=True)
        raise
    executor.shutdown(wait=True)
    return results  # type: ignore[return-value]


def format_progress(done: int, total: int, label: str, result: dict, elapsed: float) -> str:
    """標準出力へ出す1行の進捗。"""
    error = result.get("error")
    outcome = f"ERROR {error}" if error else f"{result.get('result')} {result.get('turns')}T"
    return f"[{done}/{total}] {label} -> {outcome} ({elapsed:.0f}s)"


def game_label(spec: dict) -> str:
    """対局を一意に表すラベル（ログファイル名にも使う）。"""
    return f"{spec['map']}_s{spec.get('seed')}_{spec['p1']}-{spec['p2']}"


def print_flush(line: str) -> None:
    print(line, flush=True)
    sys.stdout.flush()
