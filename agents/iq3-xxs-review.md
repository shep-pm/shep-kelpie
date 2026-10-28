---
# Kelpie's default first reviewer where paddock is set up: Strata iq3_xxs
# through the paddock gateway, reading the diff as an endpoint reviewer. It
# needs `[kelpie.gateways.paddock]` in kelpie's settings.
role: reviewer
harness: endpoint
gateway: paddock
model: iq3_xxs
context: 262144
description: Reads a diff for defects on the local paddock model, as a first pass before a stronger reviewer.
tier: light
---
