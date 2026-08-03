local QuestionForm = require("question_form")
local QuestionHelpers = require("question_helpers")
local ToolView = require("n00n.tool_view")

local DESCRIPTION =
  [[Ask the user questions during execution when a decision or missing input is required. Do not use when the answer is already known or can be inferred safely. Use `ask_user` for choices instead of guessing; put recommended options first with "(Recommended)" suffix. Supports single/multi-select, custom answers, and tabbed forms.]]

local function card_width()
  local ok, size = pcall(n00n.ui.terminal_size)
  if ok and type(size) == "table" and type(size.cols) == "number" then
    return math.max(40, size.cols - 8)
  end
  return 80
end

local function normalize_questions(questions)
  for _, q in ipairs(questions or {}) do
    q.options = q.options or {}
    q.header = q.header or ""
    q.multiple = q.multiSelect or false
  end
end

n00n.api.register_tool({
  name = "ask_user",
  aliases = { "question" },
  description = DESCRIPTION,
  -- Ask only when the user's choice is required; do not use for information already present in the request.
  schema = {
    type = "object",
    required = { "questions" },
    properties = {
      questions = {
        type = "array",
        description = "Questions requiring a user decision or missing input.",
        items = {
          type = "object",
          required = { "question" },
          properties = {
            question = { type = "string", description = "Question text shown to the user." },
            header = { type = "string", description = "Short tab header for the question." },
            options = {
              type = "array",
              description = "List of predefined options",
              items = {
                type = "object",
                required = { "label" },
                properties = {
                  label = { type = "string", description = "Option label" },
                  description = { type = "string", description = "Option description" },
                },
              },
            },
            multiSelect = {
              type = "boolean",
              description = "Whether multiple options can be selected",
              alias = "multiple",
            },
          },
        },
      },
    },
  },
  audiences = { "main" },
  timeout = false,
  header = function(input)
    local n = #input.questions
    return n .. " question" .. (n == 1 and "" or "s")
  end,
  handler = function(input, ctx)
    if #input.questions == 0 then
      return { llm_output = "error: at least one question is required", is_error = true }
    end
    normalize_questions(input.questions)
    local result = QuestionForm.open(input.questions)
    local width = card_width()
    if result.type == "dismiss" then
      return {
        llm_output = "(question dismissed by user)",
        state = { dismissed = true },
        body = QuestionHelpers.render_card(input.questions, {}, { width = width, dismissed = true }),
      }
    end
    return {
      llm_output = QuestionHelpers.format_answer_list(input.questions, result.answers),
      format = "markdown",
      state = { answers = result.answers },
      body = QuestionHelpers.render_card(input.questions, result.answers, { width = width }),
    }
  end,
  restore = function(input, output, _is_error, ctx)
    normalize_questions(input.questions)
    local state = ctx:state()
    local width = card_width()
    if state and state.answers then
      return { body = QuestionHelpers.render_card(input.questions, state.answers, { width = width }) }
    end
    if state and state.dismissed then
      return { body = QuestionHelpers.render_card(input.questions, {}, { width = width, dismissed = true }) }
    end
    return { body = ToolView.restore(output, { max_lines = 80 }) }
  end,
})
