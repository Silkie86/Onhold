/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** Origin of the API when it is not the app's own, e.g. in a desktop shell. */
  readonly VITE_API_URL?: string;
  /** `app` runs the desktop and mobile apps' mode in a plain browser, for tests. */
  readonly VITE_APP_MODE?: string;
  /** A client's own name for their branded app (see scripts/brand.mjs). */
  readonly VITE_APP_NAME?: string;
}
