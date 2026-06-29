import { describe, it } from "node:test";
import { RuleTester } from "eslint";
import tseslint from "typescript-eslint";
import rule from "./observer-boundary.js";

// Bridge ESLint's RuleTester to the node:test runner.
RuleTester.describe = describe;
RuleTester.it = it;

const ruleTester = new RuleTester({
  languageOptions: {
    parser: tseslint.parser,
    ecmaVersion: 2022,
    sourceType: "module",
    parserOptions: { ecmaFeatures: { jsx: true } },
  },
});

ruleTester.run("observer-boundary", rule, {
  valid: [
    // Read inside an <Observer> render-prop callback (nested function).
    {
      code: `
        function C() {
          const store = useStore();
          return <Observer>{() => <div>{store.status.phase}</div>}</Observer>;
        }
      `,
    },
    // Read inside useEffect (event/effect context, not render).
    {
      code: `
        function C() {
          const store = useStore();
          useEffect(() => { void store.meetings.loadDetail(1); }, [store]);
          return <div />;
        }
      `,
    },
    // Read inside an event handler.
    {
      code: `
        function C() {
          const store = useStore();
          return <button onClick={() => store.meetings.toggle()} />;
        }
      `,
    },
    // Body read inside an observer()-wrapped component.
    {
      code: `
        const C = observer(function C() {
          const store = useStore();
          return <div>{store.meetingArtifacts.templates.length}</div>;
        });
      `,
    },
    // store passed as a call argument; the read lives in a helper whose
    // `store` is a parameter (not useStore) — must not be flagged.
    {
      code: `
        function compute(store) { return store.daemonReachable; }
        function C() {
          const store = useStore();
          return <Observer>{() => <div>{compute(store)}</div>}</Observer>;
        }
      `,
    },
    // Alias destructured inside the Observer callback.
    {
      code: `
        function C() {
          const store = useStore();
          return (
            <Observer>
              {() => { const { meetings } = store; return <div>{meetings.active ? "y" : "n"}</div>; }}
            </Observer>
          );
        }
      `,
    },
    // Read inside a NAMED handler function declared in the component body and
    // wired via a JSX attribute (the post-processing.tsx onSubmit shape).
    {
      code: `
        function C() {
          const store = useStore();
          async function onSubmit() {
            await store.postProcessing.createJob();
            return store.postProcessing.lastError;
          }
          return <form onSubmit={onSubmit} />;
        }
      `,
    },
    // getRootStore() in a route action — non-reactive accessor, not tracked.
    {
      code: `
        const route = {
          action: async () => {
            const root = getRootStore();
            return root.meetings.lastError;
          },
        };
      `,
    },
  ],

  invalid: [
    // store member read directly in returned JSX, no Observer, not wrapped.
    {
      code: `
        function Bad() {
          const store = useStore();
          return <div>{store.status.phase}</div>;
        }
      `,
      errors: [{ messageId: "readOutsideObserver", data: { name: "store.status" } }],
    },
    // store member read in directly-returned JSX (chain root reported once).
    {
      code: `
        function Bad() {
          const store = useStore();
          return <ul>{store.history.entries.map((e) => <li key={e.id}>{e.text}</li>)}</ul>;
        }
      `,
      errors: [{ messageId: "readOutsideObserver", data: { name: "store.history" } }],
    },
    // Alias read at render top-level outside any boundary.
    {
      code: `
        function Bad() {
          const store = useStore();
          const { history } = store;
          return <div>{history.error}</div>;
        }
      `,
      errors: [{ messageId: "readOutsideObserver", data: { name: "history.error" } }],
    },
  ],
});
