# Issue Tracker: Fizzy

Issues and PRDs for this repo live as Fizzy cards on the Audetic board.

## Board

- Account: `6100722`
- Board: `Audetic`
- Board ID: `03ge61jjq6tmkyjv39tu6eom3`
- URL: `https://app.fizzy.do/6100722/boards/03ge61jjq6tmkyjv39tu6eom3`

## Conventions

- One unit of work is one Fizzy card.
- Use the card title as the issue title.
- Put the issue brief, acceptance criteria, and implementation notes in the card description.
- Use Fizzy steps for concrete checklist items when useful.
- Use Fizzy comments for follow-up discussion and work logs.
- Use Fizzy tags for triage state. See `triage-labels.md` for the role strings.
- Use card numbers, not internal card IDs, when running card commands.

## Common Commands

```bash
fizzy card list --account 6100722 --board 03ge61jjq6tmkyjv39tu6eom3 --all
fizzy card show <number> --account 6100722
fizzy card create --account 6100722 --board 03ge61jjq6tmkyjv39tu6eom3 --title "Title" --description "<p>Description</p>"
fizzy comment create --account 6100722 --card <number> --body "<p>Comment</p>"
fizzy card tag <number> --account 6100722 --tag ready-for-agent
fizzy card close <number> --account 6100722
```

## When a skill says "publish to the issue tracker"

Create a Fizzy card on the Audetic board. Include enough context in the description that another agent or human can pick it up without reading the full conversation.

## When a skill says "fetch the relevant ticket"

Use `fizzy card show <number> --account 6100722`. The user will normally pass the Fizzy card number directly.

## When a skill says "add a note" or "comment"

Use `fizzy comment create --account 6100722 --card <number> --body "<p>Comment</p>"`.

## When a skill says "close" or "complete"

Use `fizzy card close <number> --account 6100722` after the work is implemented and verified.

## External PRs

External pull requests are not a triage surface for this repo's agent skills. Track requested work in Fizzy unless the user explicitly asks otherwise.

## CLI Availability

If the `fizzy` shim is unavailable, do not modify global `mise` config without asking the user first. Prefer an already installed Fizzy binary or ask the user how they want the CLI activated.
