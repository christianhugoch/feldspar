/// <reference types="vite/client" />

declare module "*.css" {
  export {};
}

declare module "*?raw" {
  const content: string;
  export default content;
}
