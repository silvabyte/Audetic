# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Before exploring, read these

- **`CONTEXT-MAP.md`** at the repo root. It points at one `CONTEXT.md` per context. Read each one relevant to the topic.
- **`docs/adr/`** for system-wide architectural decisions.
- **`src/<context>/docs/adr/`** for context-scoped decisions when a context has them.

If any of these files don't exist, **proceed silently**. Don't flag their absence; don't suggest creating them upfront. The `/domain-modeling` skill creates them lazily when terms or decisions actually get resolved.

## File Structure

This repo uses a multi-context layout:

```
/
├── CONTEXT-MAP.md
├── docs/adr/                          # system-wide decisions
└── src/
    ├── <context>/
    │   ├── CONTEXT.md
    │   └── docs/adr/                  # context-specific decisions
    └── <context>/
        ├── CONTEXT.md
        └── docs/adr/
```

## Use the Glossary's Vocabulary

When your output names a domain concept in an issue title, a refactor proposal, a hypothesis, or a test name, use the term as defined in the relevant `CONTEXT.md`. Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal. Either you're inventing language the project doesn't use, or there's a real gap to note for `/domain-modeling`.

## Flag ADR Conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR-0007 (event-sourced orders), but worth reopening because..._
