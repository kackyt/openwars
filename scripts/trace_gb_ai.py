"""Gameboy Wars Turbo の実行状態をフレーム単位で記録する補助ツール。

PyBoy を明示的に導入した開発環境でだけ実行する。ROM はリポジトリに出力しない。
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
from pathlib import Path

from pyboy import PyBoy


# Bank 2 のAIが命令番号を C66C へ格納する命令アドレス。
# データ中の偶然の一致をフックしないよう、逆アセンブルで実行コードと確認できた箇所だけを列挙する。
AI_COMMAND_EMITTERS = (
    0x4064,
    0x40BA,
    0x4A75,
    0x4ADC,
    0x4B87,
    0x4BF6,
    0x4E46,
    0x4F28,
    0x4F85,
    0x5004,
    0x52D0,
    0x53E0,
    0x5675,
    0x5816,
    0x58C6,
    0x59D2,
    0x59EA,
    0x5AFF,
    0x5B51,
    0x5CF8,
    0x5DD1,
    0x5E8B,
    0x5EDF,
    0x65EC,
    0x668B,
    0x669F,
    0x66B7,
)

# 赤軍・白軍はそれぞれ16バイト固定長の部隊レコードを持つ。
UNIT_RECORD_BASES = (0xC980, 0xCC10)
UNIT_RECORD_SIZE = 16
UNIT_RECORD_COUNT = 41

# OpenWarsへ移植した5マップと、GB版マップ一覧上の実際の選択位置。
# 一覧順はOpenWarsのmap番号と一致しないため、画面上の名称と地形を照合した値を使う。
GB_MAP_SELECTION_OFFSETS = {
    "map_1": 0,  # ハムサンドトウ
    "map_2": 5,  # サイコロジマ
    "map_3": 19,  # コーラルショトウ
    "map_4": 23,  # セブンショトウ
    "map_5": 6,  # イッポンバシ
}


def gb_hex_distance_1_based(
    start_x: int, start_y: int, target_x: int, target_y: int
) -> int:
    """GBの1始まり疑似hex座標をOpenWarsと同じodd-r座標へ直して距離を求める。"""

    def to_cube(x: int, y: int) -> tuple[int, int, int]:
        zero_based_x = x - 1
        zero_based_y = y - 1
        q = zero_based_x - (zero_based_y - (zero_based_y & 1)) // 2
        return (q, zero_based_y, -q - zero_based_y)

    aq, ar, ass = to_cube(start_x, start_y)
    bq, br, bs = to_cube(target_x, target_y)
    return (abs(aq - bq) + abs(ar - br) + abs(ass - bs)) // 2


def read_attack_target_position(pyboy: PyBoy, target_index: int) -> tuple[int, int]:
    """現在の攻撃対象配列から0始まりの対象番号を座標へ変換する。"""
    if target_index == 0xFF:
        return (0, 0)
    address = 0xC56F + target_index * 2
    return (pyboy.memory[address], pyboy.memory[address + 1])


def press(pyboy: PyBoy, button: str, frames: int) -> None:
    """入力直後の画面遷移が完了するまで待つ。"""
    pyboy.button(button)
    pyboy.tick(frames)


def boot_to_stats(pyboy: PyBoy, iq: int, map_name: str) -> None:
    """指定IQ・マップで両軍C.P.のStats確認画面まで進める。

    C.P.のIQ設定は勢力ごとではなく共通である。IQ100へはC.P.を1回、
    IQ200へは2回確定する。入力手順は実ROM上で確認したもの。
    """
    pyboy.tick(240)
    press(pyboy, "start", 30)
    press(pyboy, "down", 2)
    press(pyboy, "start", 60)
    # 設定1: レッドスターをC.P.にし、共通IQを設定する。
    for button in ("up", "right", "down", "a", "down"):
        press(pyboy, button, 12)
    for _ in range(1 if iq == 100 else 2):
        press(pyboy, "a", 12)
    # C.P.項目から「セッテイオワリ」へ移動する。
    for button in ("left", "down", "a"):
        press(pyboy, button, 12)
    pyboy.tick(500)
    # 設定2を確定し、マップ一覧を先頭から指定番号まで進める。
    press(pyboy, "a", 500)
    for _ in range(GB_MAP_SELECTION_OFFSETS[map_name]):
        press(pyboy, "down", 80)
    # 一覧のスクロール中は確定入力を受け付けないため、選択表示の停止を待つ。
    pyboy.tick(300)
    # マップ選択とマップ確認を確定し、Stats画面で止める。戦闘開始はフック登録後に
    # 行わないと、先攻側の初回生産命令を取り逃す。
    for frames in (800, 800):
        press(pyboy, "a", frames)


def read_unit_records(pyboy: PyBoy) -> list[list[str]]:
    """両軍の有効な部隊レコードを、比較可能な16進表現で読む。"""
    factions: list[list[str]] = []
    for base in UNIT_RECORD_BASES:
        records = []
        for index in range(UNIT_RECORD_COUNT):
            start = base + index * UNIT_RECORD_SIZE
            record = bytes(pyboy.memory[start : start + UNIT_RECORD_SIZE])
            # 未使用レコードは先頭バイトが FF。末尾側をCSVから除外する。
            if record[0] not in (0x7F, 0xFF):
                records.append(record.hex())
        factions.append(records)
    return factions


def changed_record_indices(
    before: list[list[str]], after: list[list[str]]
) -> list[list[int]]:
    """前の命令発行時点から内容が変わった部隊レコード番号を返す。"""
    result = []
    for before_side, after_side in zip(before, after):
        changed = []
        for index in range(max(len(before_side), len(after_side))):
            old = before_side[index] if index < len(before_side) else None
            new = after_side[index] if index < len(after_side) else None
            if old != new:
                changed.append(index)
        result.append(changed)
    return result


def trace_ai_commands(pyboy: PyBoy, frames: int, output: Path) -> None:
    """AI命令の発行元と、発行時点の部隊・盤面状態をCSVへ保存する。"""
    state = {
        "frame": 0,
        "events": [],
        "previous_units": read_unit_records(pyboy),
        "previous_decision_ram": bytes(pyboy.memory[0xC500:0xC668]),
        "accepted_attack": None,
    }

    def attack_started(context: tuple[PyBoy, dict[str, object]]) -> None:
        _, trace_state = context
        trace_state["accepted_attack"] = None

    def attack_candidate_accepted(context: tuple[PyBoy, dict[str, object]]) -> None:
        emulator, trace_state = context
        target_index = emulator.memory[0xC56D]
        trace_state["accepted_attack"] = (
            emulator.register_file.B,
            emulator.register_file.C,
            read_attack_target_position(emulator, target_index),
        )

    def command_emitted(context: tuple[PyBoy, int, dict[str, object]]) -> None:
        emulator, emitter, trace_state = context
        units = read_unit_records(emulator)
        previous_units = trace_state["previous_units"]
        decision_ram = bytes(emulator.memory[0xC500:0xC668])
        previous_decision_ram = trace_state["previous_decision_ram"]
        attack_target_x = ""
        attack_target_y = ""
        attack_distance = ""
        if emitter == 0x5816 and trace_state["accepted_attack"] is not None:
            attack_x, attack_y, attack_target = trace_state["accepted_attack"]
            attack_target_x, attack_target_y = attack_target
            attack_distance = gb_hex_distance_1_based(
                attack_x, attack_y, attack_target_x, attack_target_y
            )
        decision_ram_changes = [
            index
            for index, (old, new) in enumerate(
                zip(previous_decision_ram, decision_ram)
            )
            if old != new
        ]
        trace_state["events"].append(
            {
                "frame": trace_state["frame"],
                "emitter": f"02:{emitter:04X}",
                "command": emulator.register_file.A,
                "side": emulator.memory[0xFFD6],
                "unit_index": emulator.memory[0xC669],
                "selection_scan_index": emulator.memory[0xC687],
                "selected_kind": emulator.memory[0xC4D3],
                "selected_x": emulator.memory[0xC4D4],
                "selected_y": emulator.memory[0xC4D5],
                "target_x": emulator.memory[0xC66A],
                "target_y": emulator.memory[0xC66B],
                "attack_target_x": attack_target_x,
                "attack_target_y": attack_target_y,
                "attack_distance": attack_distance,
                "argument": emulator.memory[0xC66D],
                "changed_units": json.dumps(
                    changed_record_indices(previous_units, units), separators=(",", ":")
                ),
                "decision_ram_changes": json.dumps(
                    decision_ram_changes, separators=(",", ":")
                ),
                "red_units": json.dumps(units[0], separators=(",", ":")),
                "white_units": json.dumps(units[1], separators=(",", ":")),
            }
        )
        trace_state["previous_units"] = units
        trace_state["previous_decision_ram"] = decision_ram

    for emitter in AI_COMMAND_EMITTERS:
        pyboy.hook_register(2, emitter, command_emitted, (pyboy, emitter, state))
    pyboy.hook_register(2, 0x5690, attack_started, (pyboy, state))
    pyboy.hook_register(2, 0x57F0, attack_candidate_accepted, (pyboy, state))

    pyboy.button("a")
    for frame in range(1, frames + 1):
        state["frame"] = frame
        if not pyboy.tick(1, render=False, sound=False):
            break

    fieldnames = (
        "frame",
        "emitter",
        "command",
        "side",
        "unit_index",
        "selection_scan_index",
        "selected_kind",
        "selected_x",
        "selected_y",
        "target_x",
        "target_y",
        "attack_target_x",
        "attack_target_y",
        "attack_distance",
        "argument",
        "changed_units",
        "decision_ram_changes",
        "red_units",
        "white_units",
    )
    with output.open("w", newline="", encoding="utf-8") as stream:
        writer = csv.DictWriter(stream, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(state["events"])


def trace_ai_candidates(pyboy: PyBoy, frames: int, output: Path) -> None:
    """通常移動4C19で最良値へ更新された候補と最終命令を記録する。"""
    state: dict[str, object] = {"frame": 0, "events": []}

    def candidate_considered(context: tuple[PyBoy, dict[str, object]]) -> None:
        emulator, trace_state = context
        trace_state["events"].append(
            {
                "frame": trace_state["frame"],
                "event": "considered",
                "unit_index": emulator.memory[0xC669],
                "unit_x": emulator.memory[0xC4D4],
                "unit_y": emulator.memory[0xC4D5],
                "objective_x": emulator.memory[0xC4E0],
                "objective_y": emulator.memory[0xC4E1],
                "pointer": emulator.register_file.HL,
                "primary": emulator.memory[0xC688],
                "secondary": emulator.memory[0xC689],
                "tertiary": emulator.memory[0xC68A],
                "flags": emulator.memory[0xC6A2],
            }
        )

    def candidate_accepted(context: tuple[PyBoy, dict[str, object]]) -> None:
        emulator, trace_state = context
        trace_state["events"].append(
            {
                "frame": trace_state["frame"],
                "event": "accepted",
                "unit_index": emulator.memory[0xC669],
                "unit_x": emulator.memory[0xC4D4],
                "unit_y": emulator.memory[0xC4D5],
                "objective_x": emulator.memory[0xC4E0],
                "objective_y": emulator.memory[0xC4E1],
                "candidate_x": emulator.register_file.B,
                "candidate_y": emulator.register_file.C,
                "pointer": emulator.register_file.HL,
                "primary": emulator.memory[0xC688],
                "secondary": emulator.memory[0xC689],
                "tertiary": emulator.memory[0xC68A],
                "flags": emulator.memory[0xC6A2],
            }
        )

    def wait_emitted(context: tuple[PyBoy, dict[str, object]]) -> None:
        emulator, trace_state = context
        board = [
            [emulator.memory[0xD020 + y * 31 + x] for x in range(1, 11)]
            for y in range(1, 15)
        ]
        trace_state["events"].append(
            {
                "frame": trace_state["frame"],
                "event": "command",
                "unit_index": emulator.memory[0xC669],
                "unit_x": emulator.memory[0xC4D4],
                "unit_y": emulator.memory[0xC4D5],
                "objective_x": emulator.memory[0xC4E0],
                "objective_y": emulator.memory[0xC4E1],
                "candidate_x": emulator.memory[0xC66A],
                "candidate_y": emulator.memory[0xC66B],
                "primary": emulator.memory[0xC68B],
                "secondary": emulator.memory[0xC68C],
                "tertiary": emulator.memory[0xC68D],
                "flags": emulator.memory[0xC6A2],
                "movement_costs": json.dumps(
                    list(emulator.memory[0xC4BD:0xC4C7]), separators=(",", ":")
                ),
                "board": json.dumps(board, separators=(",", ":")),
            }
        )

    pyboy.hook_register(2, 0x4C45, candidate_accepted, (pyboy, state))
    pyboy.hook_register(2, 0x4CA5, candidate_considered, (pyboy, state))
    pyboy.hook_register(2, 0x4BF6, wait_emitted, (pyboy, state))
    pyboy.button("a")
    for frame in range(1, frames + 1):
        state["frame"] = frame
        if not pyboy.tick(1, render=False, sound=False):
            break

    fieldnames = (
        "frame",
        "event",
        "unit_index",
        "unit_x",
        "unit_y",
        "objective_x",
        "objective_y",
        "candidate_x",
        "candidate_y",
        "pointer",
        "primary",
        "secondary",
        "tertiary",
        "flags",
        "movement_costs",
        "board",
    )
    with output.open("w", newline="", encoding="utf-8") as stream:
        writer = csv.DictWriter(stream, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(state["events"])


def trace_ai_attack_candidates(pyboy: PyBoy, frames: int, output: Path) -> None:
    """攻撃候補の第一比較値、最良値更新、最終命令を記録する。"""
    state: dict[str, object] = {
        "frame": 0,
        "events": [],
        "accepted_attack": None,
    }

    def append_event(
        emulator: PyBoy,
        trace_state: dict[str, object],
        event: str,
        candidate_x: int | None,
        candidate_y: int | None,
        pointer: int | None,
        primary: int,
        target_index: int,
        target_override: tuple[int, int] | None = None,
    ) -> None:
        target_x, target_y = (
            target_override
            if target_override is not None
            else read_attack_target_position(emulator, target_index)
        )
        attack_distance = ""
        if (
            candidate_x is not None
            and candidate_y is not None
            and target_x > 0
            and target_y > 0
        ):
            attack_distance = gb_hex_distance_1_based(
                candidate_x, candidate_y, target_x, target_y
            )
        trace_state["events"].append(
            {
                "frame": trace_state["frame"],
                "event": event,
                "unit_index": emulator.memory[0xC669],
                "unit_kind": emulator.memory[0xC4D3],
                "unit_x": emulator.memory[0xC4D4],
                "unit_y": emulator.memory[0xC4D5],
                "candidate_x": candidate_x,
                "candidate_y": candidate_y,
                "pointer": pointer,
                "target_index": target_index,
                "target_x": target_x,
                "target_y": target_y,
                "attack_distance": attack_distance,
                "primary": primary,
                "secondary": emulator.memory[0xC681],
                "tertiary": emulator.memory[0xC682],
                "candidate_code": emulator.memory[0xC68D],
                "reachable_value": "",
                "candidate_flags": "",
                "board_code": "",
                "target_count": emulator.memory[0xC56D],
                "movement_allowance": emulator.memory[0xC4EC],
                "attack_move_penalty": emulator.memory[0xFFDC],
            }
        )

    def append_scan_event(
        context: tuple[PyBoy, dict[str, object]], event: str
    ) -> None:
        emulator, trace_state = context
        pointer = emulator.register_file.HL
        offset = pointer - 0xD402
        candidate_x = offset % 31
        candidate_y = offset // 31
        row = {
            "frame": trace_state["frame"],
            "event": event,
            "unit_index": emulator.memory[0xC669],
            "unit_kind": emulator.memory[0xC4D3],
            "unit_x": emulator.memory[0xC4D4],
            "unit_y": emulator.memory[0xC4D5],
            "candidate_x": candidate_x,
            "candidate_y": candidate_y,
            "pointer": pointer,
            "target_index": emulator.memory[0xC56D],
            "target_x": "",
            "target_y": "",
            "attack_distance": "",
            "primary": emulator.memory[0xC680],
            "secondary": emulator.memory[0xC681],
            "tertiary": emulator.memory[0xC682],
            "candidate_code": emulator.memory[0xC68D],
            "reachable_value": emulator.memory[pointer],
            "candidate_flags": emulator.memory[pointer + 0x03E2],
            "board_code": emulator.memory[pointer - 0x03E2],
            "target_count": emulator.memory[0xC56D],
            "movement_allowance": emulator.memory[0xC4EC],
            "attack_move_penalty": emulator.memory[0xFFDC],
        }
        trace_state["events"].append(row)

    def attack_started(context: tuple[PyBoy, dict[str, object]]) -> None:
        emulator, trace_state = context
        trace_state["accepted_attack"] = None
        append_event(
            emulator,
            trace_state,
            "attack_started",
            None,
            None,
            None,
            emulator.memory[0xC680],
            emulator.memory[0xC56D],
        )

    def candidate_scanned(context: tuple[PyBoy, dict[str, object]]) -> None:
        emulator, _ = context
        # 4C60は候補列の終端でcarryを立てるため、実候補だけを記録する。
        if emulator.register_file.F & 0x10 == 0:
            append_scan_event(context, "scanned")

    def candidate_has_target_flag(context: tuple[PyBoy, dict[str, object]]) -> None:
        append_scan_event(context, "target_flagged")

    def target_list_built(context: tuple[PyBoy, dict[str, object]]) -> None:
        append_scan_event(context, "targets_built")

    def candidate_compared(context: tuple[PyBoy, dict[str, object]]) -> None:
        emulator, trace_state = context
        append_event(
            emulator,
            trace_state,
            "compared",
            None,
            None,
            emulator.register_file.HL,
            emulator.register_file.B,
            emulator.memory[0xC56D],
        )

    def candidate_accepted(context: tuple[PyBoy, dict[str, object]]) -> None:
        emulator, trace_state = context
        target_index = emulator.memory[0xC56D]
        accepted_target = read_attack_target_position(emulator, target_index)
        # 対象配列は後続候補の走査で上書きされるため、採用された瞬間の組を保持する。
        trace_state["accepted_attack"] = (
            emulator.register_file.B,
            emulator.register_file.C,
            target_index,
            accepted_target,
        )
        append_event(
            emulator,
            trace_state,
            "accepted",
            emulator.register_file.B,
            emulator.register_file.C,
            emulator.register_file.HL,
            emulator.memory[0xC680],
            target_index,
            accepted_target,
        )

    def attack_emitted(context: tuple[PyBoy, dict[str, object]]) -> None:
        emulator, trace_state = context
        accepted = trace_state.get("accepted_attack")
        if accepted is None:
            candidate_x = emulator.memory[0xC66A]
            candidate_y = emulator.memory[0xC66B]
            target_index = emulator.memory[0xC693]
            accepted_target = read_attack_target_position(emulator, target_index)
        else:
            candidate_x, candidate_y, target_index, accepted_target = accepted
        append_event(
            emulator,
            trace_state,
            "command",
            candidate_x,
            candidate_y,
            None,
            emulator.memory[0xC680],
            target_index,
            accepted_target,
        )

    pyboy.hook_register(2, 0x5690, attack_started, (pyboy, state))
    pyboy.hook_register(2, 0x5701, candidate_scanned, (pyboy, state))
    pyboy.hook_register(2, 0x5723, candidate_has_target_flag, (pyboy, state))
    pyboy.hook_register(2, 0x5733, target_list_built, (pyboy, state))
    pyboy.hook_register(2, 0x5748, candidate_compared, (pyboy, state))
    pyboy.hook_register(2, 0x57F0, candidate_accepted, (pyboy, state))
    pyboy.hook_register(2, 0x5816, attack_emitted, (pyboy, state))
    pyboy.button("a")
    for frame in range(1, frames + 1):
        state["frame"] = frame
        if not pyboy.tick(1, render=False, sound=False):
            break

    fieldnames = (
        "frame",
        "event",
        "unit_index",
        "unit_kind",
        "unit_x",
        "unit_y",
        "candidate_x",
        "candidate_y",
        "pointer",
        "target_index",
        "target_x",
        "target_y",
        "attack_distance",
        "primary",
        "secondary",
        "tertiary",
        "candidate_code",
        "reachable_value",
        "candidate_flags",
        "board_code",
        "target_count",
        "movement_allowance",
        "attack_move_penalty",
    )
    with output.open("w", newline="", encoding="utf-8") as stream:
        writer = csv.DictWriter(stream, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(state["events"])


def trace(pyboy: PyBoy, frames: int, output: Path) -> None:
    """画面更新と WRAM 更新を CSV へ保存する。"""
    previous_screen = b""
    previous_wram = bytes(pyboy.memory[0xC000:0xE000])
    with output.open("w", newline="", encoding="utf-8") as stream:
        writer = csv.DictWriter(
            stream,
            fieldnames=("frame", "pc", "rom_bank", "screen_changed", "wram_changes"),
        )
        writer.writeheader()
        pyboy.button("a")
        for frame in range(1, frames + 1):
            pyboy.tick()
            screen = pyboy.screen.ndarray.tobytes()
            wram = bytes(pyboy.memory[0xC000:0xE000])
            changes = sum(before != after for before, after in zip(previous_wram, wram))
            changed = screen != previous_screen
            if changed or changes:
                writer.writerow(
                    {
                        "frame": frame,
                        "pc": f"{pyboy.register_file.PC:04X}",
                        "rom_bank": pyboy.memory[0xFFAD],
                        "screen_changed": int(changed),
                        "wram_changes": changes,
                    }
                )
            previous_screen = screen
            previous_wram = wram


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("rom", type=Path)
    parser.add_argument("--iq", type=int, choices=(100, 200), default=200)
    parser.add_argument(
        "--map",
        choices=tuple(GB_MAP_SELECTION_OFFSETS),
        default="map_1",
        help="OpenWars側のマップ名。GB版一覧上の実位置へ変換して選択する",
    )
    parser.add_argument("--frames", type=int, default=3600)
    parser.add_argument("--output", type=Path, default=Path("reports/gb_ai_trace.csv"))
    parser.add_argument(
        "--commands",
        action="store_true",
        help="画面差分ではなくBank 2のAI命令発行を記録する",
    )
    parser.add_argument(
        "--candidates",
        action="store_true",
        help="通常移動で最良値へ更新された候補と比較値を記録する",
    )
    parser.add_argument(
        "--attack-candidates",
        action="store_true",
        help="攻撃候補の比較値と最良値更新を記録する",
    )
    arguments = parser.parse_args()

    if str(arguments.rom) == "decrypted":
        arguments.rom = next(Path("rom").glob("*-decrypted.GB"))
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    pyboy = PyBoy(str(arguments.rom), window="null", sound_emulated=False)
    try:
        pyboy.set_emulation_speed(0)
        boot_to_stats(pyboy, arguments.iq, arguments.map)
        if arguments.attack_candidates:
            trace_ai_attack_candidates(pyboy, arguments.frames, arguments.output)
        elif arguments.candidates:
            trace_ai_candidates(pyboy, arguments.frames, arguments.output)
        elif arguments.commands:
            trace_ai_commands(pyboy, arguments.frames, arguments.output)
        else:
            trace(pyboy, arguments.frames, arguments.output)
    finally:
        pyboy.stop()


if __name__ == "__main__":
    main()
