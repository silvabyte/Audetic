# MobX in web-ui: the `<Observer>` convention

This app reads MobX observable state with the **`<Observer>` render-prop**, not
the `observer()` HOC. Referenced from `NOTES.md`; enforced by the custom ESLint
rule `local/observer-boundary` (see `eslint-rules/observer-boundary.js`).

## The rule

A component that reads observable state during render MUST do the read inside an
`<Observer>{() => …}</Observer>` boundary:

```tsx
function MeetingDetail() {
  const store = useStore();
  return (
    <Observer>
      {() => <p>{store.meetings.detailStatus}</p>}
    </Observer>
  );
}
```

Keep React hooks (`useState`/`useRef`/`useEffect`) **outside** the `<Observer>` —
only the observable-reading render goes inside it. If a value derived from
observables fed a `useMemo`/`useEffect` dep, move that derivation into the
`<Observer>` render prop (and drive side-effects, e.g. `scrollIntoView`, from a
ref callback) instead of memoizing an observable read.

The legacy `observer()` HOC is also accepted (a few components in
`meeting-detail.tsx` still use it) — the lint rule treats an `observer()`-wrapped
component as already reactive.

## Why it matters

Reading observable fields (e.g. `store.postProcessing.events`) at render top-level
or in directly-returned JSX **outside** a reactive boundary means the component
won't re-render when the observable loads or changes — it only "works" when the
data happens to be present on the first render — and it spams
`[mobx] Observable read outside reactive context` warnings under strict mode.
This is a real bug, not cosmetic.

## What the lint rule covers (and doesn't)

`local/observer-boundary` flags reads off the **store** — the `useStore()` result
and its aliases (`const { meetings } = store`, `const x = store.meetings`) — when
they happen at the component's render top-level or in its returned JSX without an
`<Observer>`/`observer()` boundary.

It deliberately does **not**:

- track `getRootStore()` — that is the non-reactive accessor for route
  loaders/actions, where reading observables is correct;
- classify observable reads off arbitrary **props** (e.g. a `meeting` or `entry`
  prop) — that needs type information and would produce false positives. Reads of
  observable props still need an `<Observer>`; that part stays
  convention-enforced. (A type-checked extension is possible future work.)

Reads inside `useEffect`/`reaction`, event handlers, and other nested callbacks
are not render-time reads and are not flagged.

## Pre-existing strict-mode noise

Route loaders call store methods that read observables outside a reaction
(`observableRequiresReaction` warnings). These are benign — see `NOTES.md`.
