Use this tool sparingly and only when you truly cannot proceed without user input. Prefer making reasonable assumptions and moving forward over asking.

Valid reasons to ask:
1. Ambiguous instructions where the wrong choice would waste significant effort
2. Mutually exclusive architectural decisions with no clear winner
3. User preferences that can't be inferred from context or codebase conventions

Do NOT ask when:
- You can infer the answer from the codebase, context, or conventions
- The question is about minor details or stylistic choices
- You can make a reasonable default choice and mention it in your response
- The task is straightforward enough to just do

Usage notes:
- When `custom` is enabled (default), a "Type your own answer" option is added automatically; don't include "Other" or catch-all options
- Answers are returned as arrays of labels; set `multiple: true` to allow selecting more than one
- If you recommend a specific option, make that the first option in the list and add "(Recommended)" at the end of the label
