import { describe, expect, test } from "bun:test";
import { createElement, isValidElement, type ReactElement } from "react";
import { asError, ErrorBoundary } from "./ErrorBoundary";

type Fallback = ConstructorParameters<typeof ErrorBoundary>[0]["fallback"];

/** A boundary outside React: its state set directly, setState applied at once. */
function boundary(fallback: Fallback, resetKey?: string) {
  const b = new ErrorBoundary({ fallback, resetKey, children: "the section" });
  b.setState = ((s: object) => Object.assign(b.state, s)) as typeof b.setState;
  return b;
}

describe("ErrorBoundary", () => {
  test("whatever was thrown becomes an Error with a message", () => {
    const e = new TypeError("Cannot read properties of undefined (reading 'default')");
    expect(asError(e)).toBe(e);
    expect(asError("boom").message).toBe("boom");
    expect(asError({ code: 1 }).message).toBe("[object Object]");
    expect(ErrorBoundary.getDerivedStateFromError("bad").error?.message).toBe("bad");
  });

  test("renders the children until something throws, then the fallback made from the error", () => {
    const seen: string[] = [];
    const b = boundary((error) => {
      seen.push(error.message);
      return "crash card";
    });
    expect(b.render()).toBe("the section");
    Object.assign(b.state, ErrorBoundary.getDerivedStateFromError(new Error("voice status had no models")));
    expect(b.render()).toBe("crash card");
    expect(seen).toEqual(["voice status had no models"]);
  });

  test("Reload (reset) renders the children again", () => {
    let reload: (() => void) | null = null;
    const b = boundary((_error, reset) => {
      reload = reset;
      return "crash card";
    });
    Object.assign(b.state, ErrorBoundary.getDerivedStateFromError(new Error("x")));
    expect(b.render()).toBe("crash card");
    reload!();
    expect(b.render()).toBe("the section");
  });

  test("a plain node still works as the fallback (the pet's), and a new resetKey clears the error", () => {
    const b = boundary("fallback dog", "Mochi:idle");
    Object.assign(b.state, ErrorBoundary.getDerivedStateFromError(new Error("sprite")));
    expect(b.render()).toBe("fallback dog");
    expect(ErrorBoundary.getDerivedStateFromProps({ fallback: null, resetKey: "Mochi:idle", children: null }, b.state)).toBeNull();
    expect(ErrorBoundary.getDerivedStateFromProps({ fallback: null, resetKey: "Mochi:work", children: null }, b.state)).toEqual({ error: null, key: "Mochi:work" });
  });

  test("the fallback can be an element", () => {
    const b = boundary((error) => createElement("p", null, error.message));
    Object.assign(b.state, ErrorBoundary.getDerivedStateFromError(new Error("broken")));
    const out = b.render() as ReactElement<{ children: string }>;
    expect(isValidElement(out)).toBe(true);
    expect(out.props.children).toBe("broken");
  });
});
