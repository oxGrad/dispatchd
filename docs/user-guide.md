# Using dispatchd

A quick guide for team members to the daily standup ritual dispatchd runs
in Discord. (If you're setting up the bot itself rather than just using
it, see `docs/discord-setup.md` instead.)

## The daily ritual

Every weekday (unless the tech lead has enabled weekend runs), dispatchd
posts into a "Standup — YYYY-MM-DD" thread in the team's standup channel:

1. **Thread opens** (8:30 by default) - the day's thread is created
   quietly, before the morning ping. You can already submit `/todo` at
   this point; see "Your submissions show up in the thread" below.
2. **Morning** (9:00 by default) - everyone gets pinged in that thread and
   prompted to submit their todo(s) for the day.
3. **Afternoon** (15:00 by default) - a reminder to post progress against
   what you said you'd do.
4. **Meeting reminder** (16:00 by default) - a plain heads-up about the
   optional daily meeting; whether it actually happens is the tech lead's
   call.

If you miss a step, dispatchd nags you with an `@mention` in the thread a
while after each of the two prompts above (default 30 minutes) - once per
person per day, not on every tick. The exact times and delays are
configurable per team, so treat the above as defaults, not guarantees.

All of this happens automatically - you don't run any command to make the
thread or reminders appear. You only need the commands below to actually
submit your own todos and progress.

### Your submissions show up in the thread

`/todo`'s and `/progress`'s own replies are private (see "ephemeral" below)
- but what you submitted still shows up for everyone, because dispatchd
separately posts it into today's thread on its own, usually within a
minute or two (however often the ticker checks, which is configurable).
So after you run `/todo create`, expect to see something like:
```
📋 <@you> added a todo: **Write tests**
```
show up publicly a little after your private confirmation. Same idea for
`/progress`:
```
✅ <@you> progress on **Write tests**: Done
```
This is a one-way sync, not a live view - editing or deleting a todo after
it's already been posted to the thread doesn't change or remove that post.

## `/todo` - your task list for the day

- **`/todo create`** - opens a form: **Task** (required, short) and
  **Notes** (optional, longer). Submit it once per todo - call it again
  for a second todo, a third, etc. There's no single "todo list" input;
  each todo is its own submission.
- **`/todo list`** - shows today's todos with their ids, e.g.:
  ```
  `12` Write tests
  `13` Ship the release
  ```
  Use this when you need an id for `edit`/`delete` below.
- **`/todo edit id:<...>`** - the `id` field autocompletes over today's
  todos as you type (pick from the suggestions rather than typing a
  number by hand). Opens the same form as `create`, pre-filled with the
  current Task/Notes, so you can fix a typo or add detail.
- **`/todo delete id:<...>`** - same autocomplete. Deletes it immediately
  (no confirmation prompt) and echoes back what was deleted, so double
  check the reply if you're not sure you picked the right one. If you've
  already posted a `/progress` report against that todo, delete is
  blocked - edit it instead, or just leave it as-is.
- **`/todo help`** - a quick in-Discord reminder of the above.

## `/progress` - report progress against a todo

Two options, then a form:

- **`task`** - autocompletes over today's todos that don't have a
  progress report yet. Pick one, or ignore the suggestions and type
  something else entirely for unplanned/ad-hoc work that wasn't on your
  todo list (e.g. "Fixed a prod outage").
- **`status`** - Done / In Progress / Blocked, pick from the dropdown.

That opens a form with **Progress** (required - what actually happened)
and **Blocker** (optional - fill this in if you picked Blocked).

Posting progress against a given todo more than once shouldn't normally
be needed, but nothing stops it - if you already reported Done and now
realize there's more to do, just submit `/progress` again against the
same task.

Example - reporting against something on your todo list:
```
/progress task:"Write tests" status:Done
  → Progress: "Finished all the unit tests, all green"
  → Blocker: (left blank)
  → ✅ Progress saved: Write tests — Done
```

Example - unplanned work, nothing on your todo list matched:
```
/progress task:"Fixed a prod outage" status:Blocked
  → Progress: "Rolled back the bad deploy"
  → Blocker: "waiting on ops to confirm root cause"
  → ✅ Progress saved: Fixed a prod outage — Blocked
```

## `/team-status` - tech lead only

Shows one line per team member: how many of today's todos have a matching
progress report, e.g. `✅ Alice — 3/3 updated`, `⚠️ Budi — 1/2 updated`,
`❌ Citra — no todo posted`. Everyone else gets an "restricted to the tech
lead" reply if they try it - it's not meant as a general team overview.

## `/help` and `/ping`

`/help` lists every command in one place; `/ping` just confirms dispatchd
is online and responding.

## A few things worth knowing

- Every reply from dispatchd is **ephemeral** - only you see it, even in
  a busy thread. Nobody else sees your `/todo` or `/progress` confirmations
  (or your mistakes) - what they do see is the separate public post
  dispatchd makes into the thread shortly after (see above).
- Todos and progress reports are scoped to **today** (in the team's
  configured timezone) - you can't edit or list yesterday's todos, and a
  fresh thread starts each day.
- Everything you submit is retained for the tech lead's biweekly recap -
  there's no "delete my history," only deleting an individual todo before
  it's been reported against (see `/todo delete` above).
