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
- Keep questions clear and concise
- Provide options when there are known choices — this makes answering faster
- If you recommend a specific option, note it in the description (e.g. "Recommended")
- The user's answers will be provided in the next message
