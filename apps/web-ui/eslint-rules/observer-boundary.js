/**
 * Custom ESLint rule: observer-boundary
 *
 * Enforces the web-ui MobX convention (see apps/web-ui/feedback_mobx.md):
 * a component that reads observable state off the store during render MUST do
 * the read inside an `<Observer>{() => …}</Observer>` render-prop boundary (or
 * be wrapped in `observer()`), otherwise it won't re-render when the observable
 * changes and it trips strict-mode warnings.
 *
 * Why a custom rule and not eslint-plugin-mobx's `missing-observer`: that rule
 * assumes the `observer()` HOC and flags every capitalized component, which is
 * pure noise under this codebase's `<Observer>` render-prop convention.
 *
 * Scope (intentionally precise — "signal, no noise"):
 *   - We only track the store obtained via `useStore()` and its aliases
 *     (`const { meetings } = store`, `const artifacts = store.meetingArtifacts`).
 *     `useStore()` can only be called in render (rules-of-hooks), so its
 *     declaring function is always a component/hook that runs during render.
 *   - We do NOT track `getRootStore()` — that is the deliberate non-reactive
 *     accessor for route loaders/actions (event-handler contexts), where reading
 *     observables is correct.
 *   - We do NOT classify arbitrary observable *props*; that needs type info and
 *     would reintroduce false positives. Those stay convention-enforced.
 *
 * Detection: a store member read is a *render-time* read only when its nearest
 * enclosing function IS the component function that declared the store root.
 * Reads inside a nested function — an `<Observer>` callback, a `useEffect`/
 * `reaction`, an inline or named event handler (`onSubmit`, `onClick`), a `.map`
 * callback — sit in a different function and are skipped (they are either inside
 * a boundary or do not run during render). A render-time read is reported unless
 * the component function is `observer()`-wrapped.
 *
 * Known limitation (accepted to keep zero false positives): a store read inside
 * a callback that DOES run during render but is not a boundary (e.g. an inline
 * `.map` over a non-store array that reads the store) is not flagged. Such reads
 * are rare; the store root is usually read in the component body and caught
 * there.
 */

const STORE_HOOK = "useStore";

/** @type {import('eslint').Rule.RuleModule} */
const rule = {
  meta: {
    type: "problem",
    docs: {
      description:
        "Require store observable reads during render to be inside an <Observer> boundary",
    },
    schema: [],
    messages: {
      readOutsideObserver:
        "Observable read in render: `{{name}}` is read outside an <Observer> boundary — move the reading JSX into an <Observer>{() => …} render prop (or wrap the component in observer()).",
    },
  },

  create(context) {
    const sourceCode = context.sourceCode ?? context.getSourceCode();

    const isFunctionNode = (node) =>
      node &&
      (node.type === "FunctionDeclaration" ||
        node.type === "FunctionExpression" ||
        node.type === "ArrowFunctionExpression");

    const enclosingFunction = (node) => {
      let n = node.parent;
      while (n) {
        if (isFunctionNode(n)) return n;
        n = n.parent;
      }
      return null;
    };

    // Resolve an identifier node to its eslint-scope Variable, honoring shadowing
    // by walking the scope chain from the identifier's own scope upward.
    const resolveVariable = (idNode) => {
      let scope = sourceCode.getScope(idNode);
      while (scope) {
        const found = scope.variables.find((v) => v.name === idNode.name);
        if (found) return found;
        scope = scope.upper;
      }
      return null;
    };

    // If `variable` is the store (from useStore()) or an alias of it, return the
    // function that declares the store *root* (the useStore() call site) — that
    // is the component function. Otherwise return null.
    const storeRootFunction = (variable, seen) => {
      if (!variable || variable.defs.length !== 1) return null;
      if (seen.has(variable)) return null;
      seen.add(variable);

      const def = variable.defs[0];
      if (def.type !== "Variable" || def.node.type !== "VariableDeclarator") {
        return null;
      }
      const init = def.node.init;
      if (!init) return null;

      // const store = useStore()  → this declarator's function is the component.
      if (
        init.type === "CallExpression" &&
        init.callee.type === "Identifier" &&
        init.callee.name === STORE_HOOK
      ) {
        return enclosingFunction(def.node);
      }

      // Aliases: `const { meetings } = store` (init is Identifier) or
      // `const artifacts = store.meetingArtifacts` (init is MemberExpression).
      // Trace back to the root and use the ROOT's declaring function.
      let baseId = null;
      if (init.type === "Identifier") baseId = init;
      else if (
        init.type === "MemberExpression" &&
        init.object.type === "Identifier"
      ) {
        baseId = init.object;
      }
      if (baseId) {
        return storeRootFunction(resolveVariable(baseId), seen);
      }
      return null;
    };

    // Is the function wrapped in observer(...)? i.e. it is an argument of a
    // CallExpression whose callee is the identifier `observer`.
    const isObserverWrapped = (fnNode) => {
      const parent = fnNode.parent;
      return (
        parent &&
        parent.type === "CallExpression" &&
        parent.callee.type === "Identifier" &&
        parent.callee.name === "observer" &&
        parent.arguments.includes(fnNode)
      );
    };

    return {
      // Trigger only at the root of a member chain (object is the bare store
      // identifier), so `store.meetings.detailCache` reports once, not thrice.
      MemberExpression(node) {
        if (node.object.type !== "Identifier") return;
        const idNode = node.object;

        const variable = resolveVariable(idNode);
        const componentFn = storeRootFunction(variable, new Set());
        if (!componentFn) return; // not a store-derived read

        // Render-time read ⇔ the read sits directly in the component function
        // (its body or its returned JSX), not in a nested callback.
        if (enclosingFunction(node) !== componentFn) return;

        if (isObserverWrapped(componentFn)) return; // observer() HOC → safe

        context.report({
          node,
          messageId: "readOutsideObserver",
          data: { name: `${idNode.name}.${node.property.name ?? "…"}` },
        });
      },
    };
  },
};

export default rule;
