Use this tool when you need to ask the user questions during execution. This allows you to:
1. Gather user preferences or requirements
2. Clarify ambiguous instructions
3. Get decisions on implementation choices as you work
4. Offer choices to the user about what direction to take

Each question can have:
- A `question` field with the question text (required)
- A `header` field with a short label for tab navigation (optional)
- An `options` array with predefined choices, each having a `label` and optional `description`
- A `multiple` flag if the user can select more than one option

The user will always have the option to type a custom answer instead of selecting a predefined option.

Usage notes:
- The `question` field must be a SHORT, direct question (1-3 sentences max)
- Do NOT put analysis, explanations, or context in the question field
- Put context/reasoning in your regular text output BEFORE calling the question tool
- Provide options when there are known choices — this makes answering faster
- Always provide `options` when there are known choices
- If you recommend a specific option, note it in the description (e.g. "Recommended")
- The user's answers will be provided in the next message
