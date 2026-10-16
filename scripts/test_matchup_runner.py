import sys
import threading
import time
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from scripts import matchup_runner, run_matchup_sweep

# JSON-RPCの1行要求に、呼ばれたツール名と引数を返すだけの偽mcp-server。
FAKE_SERVER = r'''
import json, sys
for line in sys.stdin:
    request = json.loads(line)
    params = request.get("params", {})
    payload = {"tool": params.get("name"), "arguments": params.get("arguments")}
    response = {"jsonrpc": "2.0", "id": request["id"],
                "result": {"content": [{"text": json.dumps(payload)}]}}
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
'''


class FakeServerMixin:
    def setUp(self):
        self.tmp = TemporaryDirectory()
        self.server_script = Path(self.tmp.name) / "fake_server.py"
        self.server_script.write_text(FAKE_SERVER, encoding="utf-8")
        self.command = [sys.executable, str(self.server_script)]

    def tearDown(self):
        self.tmp.cleanup()


class McpServerClientTest(FakeServerMixin, unittest.TestCase):
    def test_client_starts_lazily_and_close_terminates_the_process(self):
        registry = matchup_runner.ServerRegistry()
        client = registry.create(self.command)
        self.assertIsNone(client.process, "最初の呼び出しまでは起動しない")

        reply = client.call_tool("load_map", {"map_name": "map_1"})

        self.assertEqual(reply, {"tool": "load_map", "arguments": {"map_name": "map_1"}})
        process = client.process
        registry.close_all()
        self.assertIsNotNone(process.poll(), "後始末でサーバーが終了している")

    def test_registry_closes_every_server_even_when_a_game_fails(self):
        registry = matchup_runner.ServerRegistry()
        processes = []
        lock = threading.Lock()

        def make_client(job, number):
            return registry.create(self.command)

        def run_one(job, client):
            client.call_tool("load_map")
            with lock:
                processes.append(client.process)
            if job["index"] == 3:
                raise RuntimeError("対局中の失敗")
            time.sleep(0.05)
            return {"index": job["index"]}

        jobs = [{"index": index} for index in range(6)]
        with self.assertRaises(RuntimeError):
            try:
                matchup_runner.run_jobs(
                    jobs, 3, server_key=lambda job: "same", make_client=make_client, run_one=run_one
                )
            finally:
                registry.close_all()
        time.sleep(0.2)
        self.assertTrue(processes)
        self.assertTrue(all(process.poll() is not None for process in processes))


class RunJobsTest(unittest.TestCase):
    def test_results_follow_input_order_and_each_worker_reuses_its_server(self):
        created = []
        lock = threading.Lock()

        class DummyClient:
            def close(self):
                pass

        def make_client(job, number):
            with lock:
                created.append((job["variant"], number))
            return DummyClient()

        def run_one(job, client):
            # 先に投入した対局ほど遅く終わらせ、完了順と入力順をずらす。
            time.sleep(0.02 * (5 - job["index"] % 5))
            return {"index": job["index"]}

        completed = []
        jobs = [{"index": index, "variant": "a" if index < 5 else "b"} for index in range(10)]
        results = matchup_runner.run_jobs(
            jobs,
            2,
            server_key=lambda job: job["variant"],
            make_client=make_client,
            run_one=run_one,
            on_done=lambda index, job, result, elapsed: completed.append(index),
        )

        self.assertEqual([result["index"] for result in results], list(range(10)))
        self.assertEqual(sorted(completed), list(range(10)))
        # 2本のワーカーが変種ごとに作り直すだけで、対局ごとには作らない。
        self.assertLessEqual(len(created), 4)


class SweepSummaryTest(unittest.TestCase):
    def test_parse_variants_attaches_environment_to_named_variant(self):
        with TemporaryDirectory() as tmp:
            binary = Path(tmp) / "server.exe"
            binary.write_text("", encoding="utf-8")
            variants = run_matchup_sweep.parse_variants(
                [f"base={binary}", f"noext={binary}"],
                ["noext:OPENWARS_EXT_DISABLE=v4/mod.rs::collect"],
            )
        self.assertEqual(variants["base"]["env"], {})
        self.assertEqual(variants["noext"]["env"], {"OPENWARS_EXT_DISABLE": "v4/mod.rs::collect"})
        with self.assertRaises(ValueError):
            run_matchup_sweep.parse_variants(["broken"], [])

    def test_summary_compares_action_hash_sets_per_condition(self):
        def row(variant, repeat, action_hash, result="P1_Win"):
            return {
                "map": "map_25", "seed": 1, "p1": "V4", "p2": "V200", "variant": variant,
                "repeat": repeat, "result": result, "turns": 30, "action_hash": action_hash,
            }

        rows = [
            row("base", 0, "aaaa"), row("base", 1, "bbbb"),
            row("cur", 0, "bbbb"), row("cur", 1, "aaaa"),
        ]
        summary = run_matchup_sweep.summarize(rows, ["base", "cur"])
        self.assertEqual(len(summary), 1)
        self.assertTrue(summary[0]["same_action_set"], "出現順が違っても集合が同じなら一致")

        self.assertEqual(summary[0]["verdict"], "same")

        rows.append(row("cur", 2, "cccc", result="P2_Win"))
        summary = run_matchup_sweep.summarize(rows, ["base", "cur"])
        self.assertFalse(summary[0]["same_action_set"])
        self.assertEqual(summary[0]["verdict"], "overlap", "共通の行動列があれば揺れの可能性")
        self.assertEqual(summary[0]["variants"]["cur"]["outcomes"], {"P1_Win 30T": 2, "P2_Win 30T": 1})

        disjoint = [row("base", 0, "aaaa"), row("cur", 0, "dddd")]
        self.assertEqual(run_matchup_sweep.summarize(disjoint, ["base", "cur"])[0]["verdict"], "disjoint")


if __name__ == "__main__":
    unittest.main()
