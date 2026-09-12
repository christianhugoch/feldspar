// Vendored from Saltcorn 1: packages/saltcorn-markup/types.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
import type { IWhiteList } from "xss";
import type { GeneratedTagName } from "./generated_tags.js";

export type Falsy = null | false | 0 | undefined;

export type ClassVal = string | Falsy | (string | Falsy)[];
export type StyleVal =
  | string
  | null
  | (string | Falsy)[]
  | { [key: string]: string | number | Falsy };
export type AttributeVal = string | boolean | number | undefined | null;
export type Element = string | number | boolean | null | undefined | Element[];
export type Attributes = {
  [attribute: string]: AttributeVal | ClassVal | StyleVal;
  class?: ClassVal;
  style?: StyleVal;
};

export type TagFunction = (
  attributes_or_first_child?: Attributes | Element,
  ...children: Element[]
) => string;

// Derived from html-tags via ./generated_tags so the runtime tag helpers and
// this type stay in sync (single source of truth).
type TagNames = GeneratedTagName;

type TagFunctionExports = {
  [key in TagNames]: TagFunction; // "...allTags" properties
};

export interface TagExports extends TagFunctionExports {
  genericElement: (
    tagName: string,
    attributes_or_first_child?: Attributes | Element,
    ...children: Element[]
  ) => string;
  with_curScript: (js: string) => string;
  domReady: (js: string) => string;
  text: (t: string | number, customWhiteList?: IWhiteList) => string;
  text_attr: (t: string | number) => string;
  escape: (t: string) => string;
  nbsp: string;
  mkTag: (tnm: string, voidTag?: boolean) => TagFunction;
}
