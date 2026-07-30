# Issue #58 Evaluation Report

## Reproducibility
- commit: e6e462ecac727016b3133f61f72533fa52c3eb8d
- working tree: dirty
- command: scripts/eval_matchup.py --mode batch --map map_3 --p1 V3 --p2 V3 --criteria issue58 --issue58-protocol v3-selfplay --artifact-stage baseline --seeds 58001,58002,58003,58004 --max-turns 30 --json-output benchmarks/issue-58/baseline-v3-selfplay-e6e462e.json --output benchmarks/issue-58/baseline-v3-selfplay-e6e462e.md
- seeds: 58001, 58002, 58003, 58004
- games per order: 4
- protocol: v3-selfplay
- artifact stage: baseline
- expected games: 4
- games per seed: 1
- subject / baseline: V3 / V3
- evaluator SHA-256: f8ce986afcfbbe5ea7f412cdbe1f222f4ff27ac3f78b6ffdcc8077513bcba16b
- analysis evaluator SHA-256: f8ce986afcfbbe5ea7f412cdbe1f222f4ff27ac3f78b6ffdcc8077513bcba16b
- MCP SHA-256: 59ca1df8e4d92bd7e4800088b7973b32fb3be15c86a6b5d765b63f1b44353acd
- deterministic repeatability: PASS

## Overall Result
**FAIL**

## Map and Order Criteria
| Map | Order | Games | ZOC subject / baseline | Income subject / baseline | Properties subject / baseline | Trend | External property | Complete | Result |
| --- | --- | ---: | ---: | ---: | ---: | --- | --- | --- | --- |
| map_3 | 先攻 | 4 | 81.8 / 80.8 | 24000.0 / 24000.0 | 15.0 / 15.0 | PASS | PASS | yes | PASS |

## Win Rate and Thinking Time
- map: map_3
- wins / games: 1 / 4
- win rate: 25.0%
- thinking mean / median / p95: 2323.6 / 2398.5 / 4059.3 ms

## Occupation Throughput by Seed and Order
| Map | Seed | Order | Landed capture units | Capture started | Capture completed | External gained | Retained | Lost | Landing to capture |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| map_3 | 58001 | 先攻 | 2 | 0 | 0 | 0 | 0 | 0 | - |
| map_3 | 58002 | 先攻 | 2 | 0 | 0 | 0 | 0 | 0 | - |
| map_3 | 58003 | 先攻 | 3 | 0 | 0 | 0 | 0 | 0 | - |
| map_3 | 58004 | 先攻 | 3 | 0 | 0 | 0 | 0 | 0 | - |

## Production Investment by Unit Type
| Unit type | Investment |
| --- | ---: |
| ロケットランチャー | 334800 |
| 爆撃機 | 44000 |
| 砲台 | 1480000 |
| 装甲車 | 29400 |
| 補給輸送車 | 52500 |
| 軽歩兵 | 280000 |
| 輸送ヘリ | 84000 |
| 輸送船 | 82500 |
| 重歩兵 | 146000 |

## Battleship Investment and ROI by Game
| Map | Seed | Order | Investment | Damage value | ROI |
| --- | ---: | --- | ---: | ---: | ---: |
| map_3 | 58001 | 先攻 | 0 | 0 | - |
| map_3 | 58002 | 先攻 | 0 | 0 | - |
| map_3 | 58003 | 先攻 | 0 | 0 | - |
| map_3 | 58004 | 先攻 | 0 | 0 | - |

## Invasion Milestones
| Map | Seed | Order | Transport production | Load | Drop | Combat | Capture start | Capture complete |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| map_3 | 58001 | 先攻 | 1 | 3 | 8 | 8 | - | - |
| map_3 | 58002 | 先攻 | 1 | 4 | 9 | 9 | - | - |
| map_3 | 58003 | 先攻 | 1 | 3 | 8 | 8 | - | - |
| map_3 | 58004 | 先攻 | 1 | 4 | 9 | 9 | - | - |

## Errors
- none
