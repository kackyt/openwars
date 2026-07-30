# Issue #58 Evaluation Report

## Reproducibility
- commit: e6e462ecac727016b3133f61f72533fa52c3eb8d
- working tree: dirty
- command: scripts/eval_matchup.py --mode batch --map map_1,map_2,map_3 --p1 V3 --p2 V2 --criteria issue58 --seeds 58001,58002,58003,58004 --max-turns 30 --output benchmarks/issue-58/baseline-e6e462e.md --json-output benchmarks/issue-58/baseline-e6e462e.json
- seeds: 58001, 58002, 58003, 58004
- games per order: 4
- evaluator SHA-256: 214c3303e3bf14a2996ed38354ccd01b509738e05bd5dddb485b5a8355b3e01c
- analysis evaluator SHA-256: ecde15089ed7bdd9ae4e21f0f3cf6517437d163ae5eab22ba9c3f6cea9d18123
- MCP SHA-256: 59ca1df8e4d92bd7e4800088b7973b32fb3be15c86a6b5d765b63f1b44353acd
- deterministic repeatability: FAIL

## Overall Result
**FAIL**

## Map and Order Criteria
| Map | Order | Games | ZOC subject / baseline | Income subject / baseline | Properties subject / baseline | Trend | External property | Complete | Result |
| --- | --- | ---: | ---: | ---: | ---: | --- | --- | --- | --- |
| map_1 | 先攻 | 4 | 28.5 / 40.2 | 8750.0 / 15250.0 | 5.8 / 12.2 | FAIL | PASS | yes | FAIL |
| map_1 | 後攻 | 4 | 39.5 / 28.5 | 11750.0 / 12250.0 | 8.8 / 9.2 | FAIL | PASS | yes | FAIL |
| map_2 | 先攻 | 4 | 62.5 / 13.8 | 20000.0 / 9000.0 | 14.2 / 4.8 | PASS | PASS | yes | PASS |
| map_2 | 後攻 | 4 | 55.5 / 20.8 | 16750.0 / 12250.0 | 11.8 / 7.2 | PASS | PASS | yes | PASS |
| map_3 | 先攻 | 4 | 55.5 / 80.8 | 23250.0 / 26500.0 | 14.2 / 16.8 | FAIL | PASS | yes | FAIL |
| map_3 | 後攻 | 4 | 68.8 / 106.8 | 24000.0 / 36750.0 | 15.0 / 24.2 | FAIL | FAIL | yes | FAIL |

## Win Rate and Thinking Time
- map: map_3
- wins / games: 0 / 8
- win rate: 0.0%
- thinking mean / median / p95: 2200.0 / 2220.4 / 3712.1 ms

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
| map_3 | 58001 | 先攻 | 2 | 1 | 0 | 0 | 0 | 0 | 2 |
| map_3 | 58001 | 後攻 | 3 | 0 | 0 | 0 | 0 | 0 | - |
| map_3 | 58002 | 先攻 | 2 | 0 | 0 | 0 | 0 | 0 | - |
| map_3 | 58002 | 後攻 | 3 | 0 | 0 | 0 | 0 | 0 | - |
| map_3 | 58003 | 先攻 | 2 | 0 | 0 | 0 | 0 | 0 | - |
| map_3 | 58003 | 後攻 | 3 | 1 | 0 | 0 | 0 | 0 | 2 |
| map_3 | 58004 | 先攻 | 3 | 0 | 0 | 0 | 0 | 0 | - |
| map_3 | 58004 | 後攻 | 1 | 0 | 0 | 0 | 0 | 0 | - |

## Production Investment by Unit Type
| Unit type | Investment |
| --- | ---: |
| ロケットランチャー | 1686400 |
| 対空ミサイル | 24000 |
| 対空戦車 | 22000 |
| 戦艦 | 420000 |
| 戦闘ヘリ | 127500 |
| 爆撃機 | 132000 |
| 砲台 | 4220000 |
| 装甲車 | 155400 |
| 補給輸送車 | 7000 |
| 軽戦車 | 474000 |
| 軽歩兵 | 1246000 |
| 軽自走砲 | 434000 |
| 輸送ヘリ | 112000 |
| 輸送船 | 49500 |
| 重戦車 | 420000 |
| 重歩兵 | 630000 |
| 重自走砲 | 181500 |

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
| map_2 | 58001 | 先攻 | 30000 | 17920 | 0.5973 |
| map_2 | 58001 | 後攻 | 30000 | 0 | 0.0000 |
| map_2 | 58002 | 先攻 | 0 | 0 | - |
| map_2 | 58002 | 後攻 | 120000 | 84780 | 0.7065 |
| map_2 | 58003 | 先攻 | 90000 | 52262 | 0.5807 |
| map_2 | 58003 | 後攻 | 0 | 0 | - |
| map_2 | 58004 | 先攻 | 60000 | 7564 | 0.1261 |
| map_2 | 58004 | 後攻 | 60000 | 30135 | 0.5022 |
| map_3 | 58001 | 先攻 | 0 | 0 | - |
| map_3 | 58001 | 後攻 | 30000 | 8824 | 0.2941 |
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
| map_1 | 58001 | 後攻 | - | - | - | 3 | - | - |
| map_1 | 58002 | 先攻 | - | - | - | 3 | - | - |
| map_1 | 58002 | 後攻 | - | - | - | 3 | - | - |
| map_1 | 58003 | 先攻 | - | - | - | 3 | - | - |
| map_1 | 58003 | 後攻 | - | - | - | 3 | - | - |
| map_1 | 58004 | 先攻 | - | - | - | 3 | - | - |
| map_1 | 58004 | 後攻 | - | - | - | 3 | - | - |
| map_2 | 58001 | 先攻 | 11 | - | - | 2 | - | - |
| map_2 | 58001 | 後攻 | - | - | - | 3 | - | - |
| map_2 | 58002 | 先攻 | 26 | - | - | 2 | - | - |
| map_2 | 58002 | 後攻 | 7 | - | - | 3 | - | - |
| map_2 | 58003 | 先攻 | 7 | - | - | 2 | - | - |
| map_2 | 58003 | 後攻 | 9 | - | - | 3 | - | - |
| map_2 | 58004 | 先攻 | 7 | - | - | 2 | - | - |
| map_2 | 58004 | 後攻 | - | - | - | 3 | - | - |
| map_3 | 58001 | 先攻 | 1 | 3 | 8 | 8 | 13 | - |
| map_3 | 58001 | 後攻 | 5 | 7 | 12 | 13 | - | - |
| map_3 | 58002 | 先攻 | 1 | 3 | 8 | 8 | - | - |
| map_3 | 58002 | 後攻 | 5 | 7 | 12 | 13 | - | - |
| map_3 | 58003 | 先攻 | 1 | 3 | 8 | 8 | - | - |
| map_3 | 58003 | 後攻 | 5 | 7 | 12 | 14 | 14 | - |
| map_3 | 58004 | 先攻 | 1 | 3 | 8 | 8 | - | - |
| map_3 | 58004 | 後攻 | 7 | 9 | 18 | 19 | - | - |

## Errors
- none
