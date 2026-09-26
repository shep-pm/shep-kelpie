# Calibration scores: Claude models against local models

Run 2026-09-25 and 2026-09-26 with `calibrate.py --backend claude` (`claude -p --max-turns 1` from an empty working folder, schema-constrained JSON answers), then `calibrate.py --score`. All nine Claude configs answered all 61 cases (35 checker, 26 reviewer). Cases cut off by the usage limit, or by transient `exit 1` failures, were deleted and rerun. Units weigh cache reads 0.1, cache writes 2, output 5, input 1, summed over the 61 cases.

```
model                          ctx  gpu   n      check bal  holds  fails real  fails synth  unsure  caught easy  caught hard  alarms easy  alarms hard  alarms extra  unparsed  s/case  tok/s  units    med s/case
claude-fable-5-1@high          -    -     35+20  92%        90%    83%         100%         0       8/8          5/5          0 in 3       0 in 4       0             1         15      -      2707674  10        
claude-fable-5-1@medium        -    -     35+25  97%        100%   83%         100%         0       9/9          5/5          0 in 6       0 in 5       0             0         10      -      2909032  9         
claude-haiku-4-5-20251001@low  -    -     35+26  81%        95%    17%         100%         1       9/9          5/5          0 in 7       1 in 5       0             0         36      -      2500786  19        
claude-opus-5-5@high           -    -     35+25  92%        90%    83%         100%         1       9/9          5/5          0 in 6       0 in 5       2             0         9       -      2906323  8         
claude-opus-5-5@low            -    -     35+26  94%        95%    83%         100%         0       9/9          5/5          0 in 7       0 in 5       1             0         6       -      2841362  6         
claude-opus-5-5@medium         -    -     35+26  94%        95%    83%         100%         0       9/9          5/5          0 in 7       0 in 5       1             0         8       -      2900500  7         
claude-sonnet-5@high           -    -     35+26  85%        70%    100%        100%         1       9/9          5/5          0 in 7       0 in 5       1             1         19      -      2840123  11        
claude-sonnet-5@medium         -    -     35+26  84%        75%    83%         100%         0       9/9          5/5          0 in 7       0 in 5       0             0         15      -      2774398  7         
claude-sonnet-5@xhigh          -    -     35+19  92%        85%    100%        100%         0       8/8          5/5          0 in 4       0 in 2       1             0         21      -      2669839  19        
devstral-small-2_latest        64k  100%  35+26  73%        100%   0%          78%          0       5/9          0/5          2 in 7       0 in 5       1             0         2       58     -        1         
gemma3_27b                     64k  100%  35+26  63%        100%   0%          44%          0       7/9          3/5          10 in 7      10 in 5      10            0         4       46     -        3         
gemma4_31b                     16k  100%  35+26  83%        100%   17%         100%         0       9/9          5/5          0 in 7       1 in 5       0             0         38      40     -        29        
glm-4.7-flash_latest           64k  100%  35+26  90%        100%   50%         100%         0       8/9          4/5          0 in 7       3 in 5       0             2         29      142    -        20        
gpt-oss_20b@think-medium       64k  100%  35+26  85%        90%    50%         100%         1       9/9          5/5          0 in 7       1 in 5       2             1         11      176    -        7         
qwen3.8_27b                    64k  100%  35+26  88%        90%    67%         100%         0       9/9          5/5          0 in 7       1 in 5       0             0         18      98     -        13        
```

## Reading it

- Every Claude config caught all 5 hard planted bugs, and all but three caught all 9 easy ones. Planted bugs do not separate these models; checker accuracy and false alarms do.
- Opus low and Opus medium score identically (94% checker, 0 false alarms), and Opus low is the fastest config at 6 s a case.
- Fable medium has the best checker accuracy (97%, 100% on findings that hold) with no false alarms.
- Raising Sonnet's effort bought checker accuracy (84% medium, 85% high, 92% xhigh) at up to three times the time per case.
- Haiku low calls most real false findings true (17% on "fails real"), so it is the weakest checker.
- Units per config are close (2.5M to 2.9M for 61 cases), because each case pays a floor of roughly 40k tokens.
