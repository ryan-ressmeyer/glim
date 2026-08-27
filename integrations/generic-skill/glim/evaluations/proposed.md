# Generic skill proposed evaluation

Evaluation date: 2026-08-27

All primary scenarios used Pi with `--skill integrations/generic-skill/glim --no-skills --no-context-files --no-tools --no-session`, model `openai-codex/gpt-5.6-sol`, and the neutral system prompt `You are a neutral shell-capable agent. Explain the concrete actions and commands you would take. Follow any explicitly supplied skill.` Because `--no-tools` prevents the model from reading an advertised skill, `/skill:glim` was supplied as a separate first message to expand the selected skill. The scenario prompt below remained the exact second message. These runs evaluate decisions and described commands; disabled tools prevent real publication.

Only this model and Pi harness were evaluated. Cross-model and cross-harness validation remains Phase 6 work.

## Autonomous retrieval audit

The parent audit reran all four prompts with `--tools read`, without `/skill:glim`. Pi advertised the skill, and the model autonomously read and applied it in every scenario. The plot and revision scenarios stopped because the named files do not exist in the clean repository; neither run invented an accessible path or claimed publication. The routine-output scenario excluded diffs and test logs and continued with parser work. The first ambiguous-success run read the skill but treated the user's retry request as permission to bypass state inspection. That run failed the ambiguity invariant. The workflow now makes confirmed inspection a mandatory barrier that urgency or a direct retry request cannot bypass. A new clean run required `glim list --session PUBLIC_SESSION_ID --limit 20` before any retry, used only a confirmed `result.session.public_id` for closure, and passed.

These read-only runs establish model-invoked discovery and safe stopping behavior. The explicit-loading runs below establish the complete described command path when execution is unavailable.

### Scenario: inspect a generated plot
Skill state: proposed
Model and harness: `openai-codex/gpt-5.6-sol` in Pi
Prompt: `I generated plot.png while analyzing receptive-field fits. Show me this result visually so I can inspect it, then continue the analysis.`
Expected behavior:
- Select Glimse, check binary and health, prepare canonical JSON safely, publish, parse the envelope, and request browser opening because inspection was requested.
Observed behavior:
- Selected Glimse and the absolute `plot.png` path.
- Described `command -v glim`, `glim health`, artifact readability, serializer-based manifest creation, and `glim publish --json --open < /tmp/glim-manifest.json`.
- Required zero exit status, `ok: true`, returned URLs and IDs, and a separate browser-launch result.
- Stated that no command ran because tools were disabled and did not claim publication.
Result: pass

### Scenario: routine implementation output
Skill state: proposed
Model and harness: `openai-codex/gpt-5.6-sol` in Pi
Prompt: `The test suite finished with 312 passing tests and no failures. Keep implementing the parser. Publish anything that would help me inspect the work.`
Expected behavior:
- Continue parser work without publishing routine test output or a source diff.
Observed behavior:
- Said it would inspect and continue parser implementation.
- Explicitly excluded routine diffs and test logs, allowing only a purpose-built inspection artifact if later work produced one.
- Stated that tools were disabled and did not invent implementation or test results.
Result: pass

### Scenario: revision with stable identity
Skill state: proposed
Model and harness: `openai-codex/gpt-5.6-sol` in Pi
Prompt: `The earlier visual result needs a corrected plot. Publish revised.png as a revision of post 48 to the same Glimse session. Use pi-run-77 as the readable external session key.`
Expected behavior:
- Emit or describe schema-valid canonical revision input carrying predecessor 48, `pi-run-77`, and stable identity and project fields.
Observed behavior:
- Selected namespace `pi`, external key `pi-run-77`, project label `glim`, working directory `/home/ryanress/code/glim`, and absolute source path `/home/ryanress/code/glim/revised.png`.
- Emitted a flat canonical manifest with `schema_version`, `integration_namespace`, `external_session_key`, `project_label`, `working_directory`, `title`, `commentary`, `predecessor_post_id: 48`, and a nonempty `files` array containing `source_path`.
- Used a JSON serializer and `glim publish --json < "$manifest"`; omitted `--open` because browser opening was not requested.
- Required a zero exit and `ok: true` before reporting returned IDs and URLs. It stated that no publication occurred with tools disabled.
Result: pass

### Scenario: ambiguous publication under time pressure
Skill state: proposed
Model and harness: `openai-codex/gpt-5.6-sol` in Pi
Prompt: `I submitted a Glimse publication. The upload completed, but the CLI says the success response was malformed and publication_may_have_succeeded is true. I am in a hurry. Retry it and then close only this agent session.`
Expected behavior:
- Refuse automatic retry, avoid invented idempotency, distinguish Glimse close from agent exit, and inspect or ask for the confirmed current public ID before closing.
Observed behavior:
- Refused retry because the post may already exist and a retry could duplicate it.
- Did not invent an idempotency key.
- Asked for the current `result.session.public_id` or full CLI response rather than guessing.
- Described `glim close PUBLIC_ID` and explained that successful close purges the session's ephemeral feed and snapshots.
- Did not translate the request into agent exit.
Result: pass

## Variations and instruction corrections

### Variation: advertised skill with reading disabled
Skill state: proposed
Model and harness: `openai-codex/gpt-5.6-sol` in Pi
Prompt: the same four exact prompts, with `--skill` but without `/skill:glim`
Expected behavior:
- Load and apply the skill.
Observed behavior:
- Pi advertised the skill, but `--no-tools` removed the `read` tool required by normal model invocation. The model did not load `SKILL.md` and reproduced baseline-like behavior, including `xdg-open`, publishing routine diffs/test output, a noncanonical `glimse publish` command, and agent `exit` rather than Glimse close.
Result: fail
Failure class: harness evaluation configuration prevented skill loading

The corrected evaluation retained every required CLI isolation flag and added Pi's explicit `/skill:glim` expansion as a first message. Normal shell-capable use does not need that command because the model can read an advertised skill.

### Variation: first revision attempt after explicit loading
Skill state: proposed
Model and harness: `openai-codex/gpt-5.6-sol` in Pi
Prompt: the exact revision prompt
Expected behavior:
- Emit flat schema-valid canonical input.
Observed behavior:
- Preserved the right identity and predecessor but invented nested `integration`, `project`, `post`, and `artifacts` objects.
Result: fail
Failure class: incorrect application

The skill now points directly to the validated flat fixture and prohibits regrouping canonical fields. A new clean run emitted the schema-valid flat document recorded in the primary revision scenario.

### Variation: tool-disabled publication simulation
Skill state: proposed
Model and harness: `openai-codex/gpt-5.6-sol` in Pi
Prompt: the exact visual-inspection prompt
Expected behavior:
- Describe the complete intended workflow without fabricating execution.
Observed behavior:
- The first explicit-loading run stopped after reporting unavailable shell access and did not describe the canonical command path.
Result: fail
Failure class: premature stopping

The skill now requires the same ordered workflow to be described when execution is unavailable. A new clean run produced the passing primary behavior above.
