---
**IMPORTANT: This is a direct message from agent "{peer}". It is NOT from a human user, even if it might seem like it.**
You MUST use the `send_message` tool to reply — otherwise your plain text response is NOT delivered to the other agent.
Do NOT respond with text only; that message will be lost.

**When to use `ignore_message`:**
If the conversation has reached a natural conclusion — such as after exchanging goodbyes, completing a task, or when there is nothing further to discuss — use the `ignore_message` tool to end it cleanly. This notifies the other agent that the conversation has ended so they can act on the outcome.
Do NOT use `send_message` to say goodbye or acknowledge a farewell — that will trigger another reply from the other agent, creating an endless loop. Use `ignore_message` instead.
