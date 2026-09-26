# Calibration scores: Claude models against local models

Run 2026-09-25 with `calibrate.py --backend claude` (`claude -p --max-turns 1`, empty working folder), then `calibrate.py --score`. The week's usage limit cut the run short: cases that failed with `exit 1` count as unparsed below, so only complete configs compare cleanly.

| config | usable cases of 61 |
|---|---|
| claude-fable-5-1@high | 40 |
| claude-fable-5-1@medium | 52 |
| claude-haiku-4-5-20251001@low | 37 |
| claude-opus-5-5@high | 58 |
| claude-opus-5-5@low | 61 |
| claude-opus-5-5@medium | 61 |
| claude-sonnet-5@high | 47 |
| claude-sonnet-5@medium | 40 |
| claude-sonnet-5@xhigh | 44 |

```
model                          ctx  gpu   n      check bal  holds  fails real  fails synth  unsure  caught easy  caught hard  alarms easy  alarms hard  alarms extra  unparsed  s/case  tok/s  units    med s/case
claude-fable-5-1@high          -    -     35+26  92%        90%    83%         100%         0       5/9          0/5          0 in 7       0 in 5       0             22        8       -      1906884  7         
claude-fable-5-1@medium        -    -     35+26  97%        100%   83%         100%         0       8/9          3/5          0 in 7       0 in 5       0             9         11      -      2480095  7         
claude-haiku-4-5-20251001@low  -    -     35+26  81%        95%    17%         100%         1       2/9          0/5          0 in 7       0 in 5       0             24        13      -      1159473  10        
claude-opus-5-5@high           -    -     35+26  92%        90%    83%         100%         1       8/9          5/5          0 in 7       0 in 5       2             3         10      -      2802655  8         
claude-opus-5-5@low            -    -     35+26  94%        95%    83%         100%         0       9/9          5/5          0 in 7       0 in 5       1             0         6       -      2841362  6         
claude-opus-5-5@medium         -    -     35+26  94%        95%    83%         100%         0       9/9          5/5          0 in 7       0 in 5       1             0         8       -      2900500  7         
claude-sonnet-5@high           -    -     35+26  85%        70%    100%        100%         1       9/9          0/5          0 in 7       0 in 5       1             15        18      -      2124368  8         
claude-sonnet-5@medium         -    -     35+26  81%        75%    83%         89%          0       6/9          0/5          0 in 7       0 in 5       0             21        18      -      1773863  6         
claude-sonnet-5@xhigh          -    -     35+26  92%        85%    100%        100%         0       8/9          0/5          0 in 7       0 in 5       1             17        21      -      2102759  14        
devstral-small-2_latest        64k  100%  35+26  73%        100%   0%          78%          0       5/9          0/5          2 in 7       0 in 5       1             0         2       58     -        1         
gemma3_27b                     64k  100%  35+26  63%        100%   0%          44%          0       7/9          3/5          10 in 7      10 in 5      10            0         4       46     -        3         
gemma4_31b                     16k  100%  35+26  83%        100%   17%         100%         0       9/9          5/5          0 in 7       1 in 5       0             0         38      40     -        29        
glm-4.7-flash_latest           64k  100%  35+26  90%        100%   50%         100%         0       8/9          4/5          0 in 7       3 in 5       0             2         29      142    -        20        
gpt-oss_20b@think-medium       64k  100%  35+26  85%        90%    50%         100%         1       9/9          5/5          0 in 7       1 in 5       2             1         11      176    -        7         
qwen3.8_27b                    64k  100%  35+26  88%        90%    67%         100%         0       9/9          5/5          0 in 7       1 in 5       0             0         18      98     -        13        
```
