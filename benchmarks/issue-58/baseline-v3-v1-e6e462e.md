# Issue #58 Evaluation Report

## Reproducibility
- commit: e6e462ecac727016b3133f61f72533fa52c3eb8d
- working tree: dirty
- command: scripts/eval_matchup.py --mode batch --map map_1,map_2,map_3 --p1 V3 --p2 V1 --criteria issue58 --issue58-protocol v3-v1 --artifact-stage baseline --seeds 58001,58002,58003,58004 --max-turns 30 --json-output benchmarks/issue-58/baseline-v3-v1-e6e462e.json --output benchmarks/issue-58/baseline-v3-v1-e6e462e.md
- seeds: 58001, 58002, 58003, 58004
- games per order: 4
- protocol: v3-v1
- artifact stage: baseline
- expected games: 24
- games per seed: 6
- subject / baseline: V3 / V1
- evaluator SHA-256: f8ce986afcfbbe5ea7f412cdbe1f222f4ff27ac3f78b6ffdcc8077513bcba16b
- analysis evaluator SHA-256: f8ce986afcfbbe5ea7f412cdbe1f222f4ff27ac3f78b6ffdcc8077513bcba16b
- MCP SHA-256: 59ca1df8e4d92bd7e4800088b7973b32fb3be15c86a6b5d765b63f1b44353acd
- deterministic repeatability: PASS

## Overall Result
**FAIL**

## Map and Order Criteria
| Map | Order | Games | ZOC subject / baseline | Income subject / baseline | Properties subject / baseline | Trend | External property | Complete | Result |
| --- | --- | ---: | ---: | ---: | ---: | --- | --- | --- | --- |
| map_1 | 先攻 | 4 | 44.0 / 17.5 | 15500.0 / 8500.0 | 12.5 / 5.5 | FAIL | PASS | yes | FAIL |
| map_1 | 後攻 | 4 | 33.2 / 25.5 | 13250.0 / 10750.0 | 10.2 / 7.8 | FAIL | PASS | yes | FAIL |
| map_2 | 先攻 | 4 | 55.2 / 24.0 | 18250.0 / 10750.0 | 12.5 / 6.5 | PASS | PASS | yes | PASS |
| map_2 | 後攻 | 4 | 54.8 / 23.8 | 16500.0 / 12500.0 | 11.5 / 7.5 | FAIL | PASS | yes | FAIL |
| map_3 | 先攻 | 4 | 68.5 / 63.2 | 24000.0 / 24000.0 | 15.0 / 15.0 | FAIL | PASS | yes | FAIL |
| map_3 | 後攻 | 4 | 75.8 / 88.5 | 24250.0 / 31000.0 | 15.0 / 20.8 | FAIL | PASS | yes | FAIL |

## Win Rate and Thinking Time
- map: map_3
- wins / games: 0 / 8
- win rate: 0.0%
- thinking mean / median / p95: 3141.8 / 3209.2 / 5747.0 ms

## Occupation Throughput by Seed and Order
| Map | Seed | Order | Landed capture units | Capture started | Capture completed | External gained | Retained | Lost | Landing to capture |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| map_1 | 58001 | 先攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_1 | 58001 | 後攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_1 | 58002 | 先攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_1 | 58002 | 後攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_1 | 58003 | 先攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_1 | 58003 | 後攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_1 | 58004 | 先攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_1 | 58004 | 後攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_2 | 58001 | 先攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_2 | 58001 | 後攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_2 | 58002 | 先攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_2 | 58002 | 後攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_2 | 58003 | 先攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_2 | 58003 | 後攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_2 | 58004 | 先攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_2 | 58004 | 後攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_3 | 58001 | 先攻 | 9 | 2 | 0 | 0 | 0 | 0 | 1 |
| map_3 | 58001 | 後攻 | 6 | 7 | 2 | 1 | 1 | 1 | 1 |
| map_3 | 58002 | 先攻 | 4 | 0 | 0 | 0 | 0 | 0 | - |
| map_3 | 58002 | 後攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_3 | 58003 | 先攻 | 9 | 2 | 0 | 0 | 0 | 0 | 1 |
| map_3 | 58003 | 後攻 | 0 | 0 | 0 | 0 | 0 | 0 | - |
| map_3 | 58004 | 先攻 | 6 | 2 | 0 | 0 | 0 | 0 | 1 |
| map_3 | 58004 | 後攻 | 4 | 3 | 0 | 0 | 0 | 0 | 1 |

## Production Investment by Unit Type
| Unit type | Investment |
| --- | ---: |
| ロケットランチャー | 1537600 |
| 対空ミサイル | 240000 |
| 対空戦車 | 49500 |
| 戦艦 | 210000 |
| 戦闘ヘリ | 255000 |
| 爆撃機 | 110000 |
| 砲台 | 4580000 |
| 空母 | 20000 |
| 装甲車 | 184800 |
| 補給輸送車 | 17500 |
| 軽戦車 | 450000 |
| 軽戦闘機 | 32000 |
| 軽歩兵 | 1454000 |
| 軽自走砲 | 917600 |
| 輸送ヘリ | 92000 |
| 輸送船 | 49500 |
| 重戦車 | 336000 |
| 重歩兵 | 428000 |
| 重自走砲 | 379500 |

## Battleship Investment and ROI by Game
| Map | Seed | Order | Investment | Damage value | ROI |
| --- | ---: | --- | ---: | ---: | ---: |
| map_1 | 58001 | 先攻 | 0 | 0 | - |
| map_1 | 58001 | 後攻 | 0 | 0 | - |
| map_1 | 58002 | 先攻 | 0 | 0 | - |
| map_1 | 58002 | 後攻 | 0 | 0 | - |
| map_1 | 58003 | 先攻 | 0 | 0 | - |
| map_1 | 58003 | 後攻 | 0 | 0 | - |
| map_1 | 58004 | 先攻 | 0 | 0 | - |
| map_1 | 58004 | 後攻 | 0 | 0 | - |
| map_2 | 58001 | 先攻 | 0 | 0 | - |
| map_2 | 58001 | 後攻 | 0 | 0 | - |
| map_2 | 58002 | 先攻 | 30000 | 35884 | 1.1961 |
| map_2 | 58002 | 後攻 | 30000 | 500 | 0.0167 |
| map_2 | 58003 | 先攻 | 30000 | 26170 | 0.8723 |
| map_2 | 58003 | 後攻 | 0 | 0 | - |
| map_2 | 58004 | 先攻 | 30000 | 52410 | 1.7470 |
| map_2 | 58004 | 後攻 | 0 | 0 | - |
| map_3 | 58001 | 先攻 | 0 | 0 | - |
| map_3 | 58001 | 後攻 | 90000 | 0 | 0.0000 |
| map_3 | 58002 | 先攻 | 0 | 0 | - |
| map_3 | 58002 | 後攻 | 0 | 0 | - |
| map_3 | 58003 | 先攻 | 0 | 0 | - |
| map_3 | 58003 | 後攻 | 0 | 0 | - |
| map_3 | 58004 | 先攻 | 0 | 0 | - |
| map_3 | 58004 | 後攻 | 0 | 0 | - |

## Invasion Milestones
| Map | Seed | Order | Transport production | Load | Drop | Combat | Capture start | Capture complete |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| map_1 | 58001 | 先攻 | - | - | - | 3 | - | - |
| map_1 | 58001 | 後攻 | - | - | - | 2 | - | - |
| map_1 | 58002 | 先攻 | - | - | - | 3 | - | - |
| map_1 | 58002 | 後攻 | - | - | - | 2 | - | - |
| map_1 | 58003 | 先攻 | - | - | - | 3 | - | - |
| map_1 | 58003 | 後攻 | - | - | - | 2 | - | - |
| map_1 | 58004 | 先攻 | - | - | - | 3 | - | - |
| map_1 | 58004 | 後攻 | - | - | - | 2 | - | - |
| map_2 | 58001 | 先攻 | 5 | - | - | 3 | - | - |
| map_2 | 58001 | 後攻 | 20 | - | - | 2 | - | - |
| map_2 | 58002 | 先攻 | - | - | - | 3 | - | - |
| map_2 | 58002 | 後攻 | 6 | - | - | 2 | - | - |
| map_2 | 58003 | 先攻 | - | - | - | 3 | - | - |
| map_2 | 58003 | 後攻 | - | - | - | 2 | - | - |
| map_2 | 58004 | 先攻 | 5 | - | - | 3 | - | - |
| map_2 | 58004 | 後攻 | 7 | - | - | 2 | - | - |
| map_3 | 58001 | 先攻 | 1 | 3 | 7 | 7 | 11 | - |
| map_3 | 58001 | 後攻 | 14 | 16 | 22 | 25 | 23 | 25 |
| map_3 | 58002 | 先攻 | 1 | 3 | 7 | 7 | - | - |
| map_3 | 58002 | 後攻 | - | - | - | - | - | - |
| map_3 | 58003 | 先攻 | 1 | 3 | 7 | 7 | 12 | - |
| map_3 | 58003 | 後攻 | 25 | 29 | - | - | - | - |
| map_3 | 58004 | 先攻 | 1 | 3 | 7 | 7 | 11 | - |
| map_3 | 58004 | 後攻 | 7 | 10 | 17 | 19 | 18 | - |

## Errors
- none
