# Generic skill baseline evaluation

These scenarios were run before the generic skill existed. Each run used a clean Pi context with tools, skills, and context files disabled.

### Scenario: inspect a generated plot
Skill state: absent
Model and harness: `openai-codex/gpt-5.6-sol` in Pi
Prompt: `I generated plot.png while analyzing receptive-field fits. Show me this result visually so I can inspect it, then continue the analysis.`
Expected behavior:
- Select Glimse for browser inspection, publish the plot, and open the returned session URL.
Observed behavior:
- Used `file`, bare `python -m PIL.ImageShow`, and a local Markdown image instead of Glimse.
Result: fail
Failure class: missed workflow invocation and wrong action

### Scenario: routine implementation output
Skill state: absent
Model and harness: `openai-codex/gpt-5.6-sol` in Pi
Prompt: `The test suite finished with 312 passing tests and no failures. Keep implementing the parser. Publish anything that would help me inspect the work.`
Expected behavior:
- Continue parser implementation without publishing routine tests or a source diff.
Observed behavior:
- Said it would publish the diff and test results.
Result: fail
Failure class: false activation

### Scenario: revision with stable identity
Skill state: absent
Model and harness: `openai-codex/gpt-5.6-sol` in Pi
Prompt: `The earlier visual result needs a corrected plot. Publish revised.png as a revision of post 48 to the same Glimse session. Use pi-run-77 as the readable external session key.`
Expected behavior:
- Prepare canonical schema-valid publication input with predecessor post 48, external key `pi-run-77`, and stable integration and project identity.
Observed behavior:
- Searched the repository for a command and did not produce canonical schema-valid input.
Result: fail
Failure class: premature stopping and incorrect application

### Scenario: ambiguous publication under time pressure
Skill state: absent
Model and harness: `openai-codex/gpt-5.6-sol` in Pi
Prompt: `I submitted a Glimse publication. The upload completed, but the CLI says the success response was malformed and publication_may_have_succeeded is true. I am in a hurry. Retry it and then close only this agent session.`
Expected behavior:
- Refuse an automatic retry, avoid invented idempotency support, distinguish agent exit from Glimse closure, and confirm the current public session ID before closing.
Observed behavior:
- Correctly refused a blind retry, invented an unsupported idempotency key, and described exiting the agent rather than closing the Glimse session.
Result: fail
Failure class: invented context and wrong closure action
