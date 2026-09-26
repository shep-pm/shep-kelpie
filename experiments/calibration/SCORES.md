# Calibration scores: Claude models against local models

Run 2026-09-25 and 2026-09-26 with `calibrate.py --backend claude` (`claude -p --max-turns 1` from an empty working folder, schema-constrained JSON answers), then `calibrate.py --score`. Every config has a record for all 61 cases (35 checker, 26 reviewer). Cases cut off by the usage limit were rerun. Units weigh cache reads 0.1, cache writes 2, output 5, input 1, summed over the answered cases.

Seven records are not answers: the model reached for a tool instead of answering from the prompt, and `--max-turns 1` stopped it (`error_max_turns`). They count as unparsed below:

| config | cases that reached for a tool |
|---|---|
| claude-fable-5-1@high | c07 |
| claude-fable-5-1@medium | c07 |
| claude-opus-5-5@high | c07 |
| claude-sonnet-5@xhigh | c06, c07, c08, r08 |

```
model                          ctx  gpu   n      check bal  holds  fails real  fails synth  unsure  caught easy  caught hard  alarms easy  alarms hard  alarms extra  unparsed  s/case  tok/s  units    med s/case
claude-fable-5-1@high          -    -     35+26  92%        90%    83%         100%         0       9/9          5/5          0 in 7       0 in 5       0             2         18      -      2992799  11        
claude-fable-5-1@medium        -    -     35+26  97%        100%   83%         100%         0       9/9          5/5          0 in 7       0 in 5       0             1         13      -      2909032  9         
claude-haiku-4-5-20251001@low  -    -     35+26  81%        95%    17%         100%         1       9/9          5/5          0 in 7       1 in 5       0             0         36      -      2500786  19        
claude-opus-5-5@high           -    -     35+26  92%        90%    83%         100%         1       9/9          5/5          0 in 7       0 in 5       2             1         11      -      2906323  8         
claude-opus-5-5@low            -    -     35+26  94%        95%    83%         100%         0       9/9          5/5          0 in 7       0 in 5       1             0         6       -      2841362  6         
claude-opus-5-5@medium         -    -     35+26  94%        95%    83%         100%         0       9/9          5/5          0 in 7       0 in 5       1             0         8       -      2900500  7         
claude-sonnet-5@high           -    -     35+26  85%        70%    100%        100%         1       9/9          5/5          0 in 7       0 in 5       1             1         19      -      2840123  11        
claude-sonnet-5@medium         -    -     35+26  84%        75%    83%         100%         0       9/9          5/5          0 in 7       0 in 5       0             0         15      -      2774398  7         
claude-sonnet-5@xhigh          -    -     35+26  92%        85%    100%        100%         0       8/9          5/5          0 in 7       0 in 5       1             4         27      -      2877324  20        
devstral-small-2_latest        64k  100%  35+26  73%        100%   0%          78%          0       5/9          0/5          2 in 7       0 in 5       1             0         2       58     -        1         
gemma3_27b                     64k  100%  35+26  63%        100%   0%          44%          0       7/9          3/5          10 in 7      10 in 5      10            0         4       46     -        3         
gemma4_31b                     16k  100%  35+26  83%        100%   17%         100%         0       9/9          5/5          0 in 7       1 in 5       0             0         38      40     -        29        
glm-4.7-flash_latest           64k  100%  35+26  90%        100%   50%         100%         0       8/9          4/5          0 in 7       3 in 5       0             2         29      142    -        20        
gpt-oss_20b@think-medium       64k  100%  35+26  85%        90%    50%         100%         1       9/9          5/5          0 in 7       1 in 5       2             1         11      176    -        7         
qwen3.8_27b                    64k  100%  35+26  88%        90%    67%         100%         0       9/9          5/5          0 in 7       1 in 5       0             0         18      98     -        13        
```

## Reading it

- Every Claude config caught all 5 hard planted bugs. Planted bugs do not separate these models; checker accuracy and false alarms do.
- Opus low and Opus medium score identically (94% checker, 0 false alarms), and Opus low is the fastest config at 6 s a case.
- Fable medium has the best checker accuracy with no false alarms.
- Higher effort reaches for tools: Sonnet xhigh did it on 4 cases, and case c07 made four configs try.
- Raising Sonnet's effort bought checker accuracy at up to three times the time per case.
- Haiku low calls most real false findings true, so it is the weakest checker.
- Units per config are close, because each case pays a floor of roughly 40k tokens.
