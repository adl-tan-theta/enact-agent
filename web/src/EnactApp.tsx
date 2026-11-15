import App from "./App";

// Keep the Enact entry point separate from upstream's App implementation.
// Enact-specific identity and presentation belong in the overlay assets and
// branding helpers; this wrapper keeps future upstream App changes flowing
// through without maintaining a forked copy of the application shell.
export { ErrorBoundary, LocaleContext, useLocaleContext } from "./App";

export default function EnactApp() {
  return <App />;
}
