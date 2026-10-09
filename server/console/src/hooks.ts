import { useEffect, useReducer, useRef } from "react";
import { store, type State } from "./store";

/**
 * 订阅 store 的一个切片。
 *
 * 只有选择器结果**变了**才重渲染——这是扛住 60x 档 16ms/帧的关键：
 * 时钟订阅 `sim_minute`，地图和列表订阅快照，互不干扰。
 *
 * 选择器可以是每次渲染新建的箭头函数，内部用 ref 固定，不会重复订阅。
 */
export function useStore<T>(selector: (s: State) => T): T {
  const [, force] = useReducer((c: number) => c + 1, 0);
  const selRef = useRef(selector);
  selRef.current = selector;

  const value = selector(store.getState());
  const valRef = useRef(value);
  valRef.current = value;

  useEffect(
    () =>
      store.subscribe(() => {
        const next = selRef.current(store.getState());
        if (!Object.is(next, valRef.current)) {
          valRef.current = next;
          force();
        }
      }),
    [],
  );

  return value;
}

/** 当前是否控制端（观战者只能看）。 */
export function useIsControl(): boolean {
  return useStore((s) => s.role === "control");
}
