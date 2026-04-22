// electron-vite's `?asset` suffix copies files into the bundle and returns
// their runtime path as a string. Declare the pattern so TS accepts imports.
declare module "*?asset" {
  const path: string;
  export default path;
}
