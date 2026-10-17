"""AI層の逆依存検査が実コードだけを数えることを検証する。"""

import tempfile
import unittest
from pathlib import Path

from scripts.measure_ai_structure import measure


class ReverseDependencyTests(unittest.TestCase):
    def test_forbidden_references_are_reported_but_tests_and_runtime_peers_are_not(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "tactics").mkdir()
            (root / "runtime").mkdir()
            (root / "context").mkdir()
            (root / "tactics" / "select.rs").write_text(
                "use crate::ai::engine::AiCommand;\n"
                "use crate::ai::runtime::issue::execute_ai_command;\n"
                "// crate::ai::engine::ignored\n"
                "#[cfg(test)]\nmod tests {\n"
                "    use crate::ai::runtime::state::AiActionCooldown;\n}\n"
            )
            (root / "runtime" / "turn.rs").write_text(
                "use crate::ai::runtime::issue::execute_ai_command;\n"
                "use crate::ai::engine::AiCommand;\n"
            )
            (root / "context" / "board.rs").write_text(
                "use crate::ai::command::AiCommand;\n"
            )
            result = measure(root)
            self.assertEqual(result["reverse_dependencies"], 3)
            self.assertEqual(
                result["reverse_dependencies_by_direction"],
                {
                    "runtime->engine": ["runtime/turn.rs:2"],
                    "tactics->engine": ["tactics/select.rs:1"],
                    "tactics->runtime": ["tactics/select.rs:2"],
                },
            )


if __name__ == "__main__":
    unittest.main()
