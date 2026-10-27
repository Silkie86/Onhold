/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** Origin of the API when it is not the app's own, e.g. in a desktop shell. */
  readonly VITE_API_URL?: string;
}
