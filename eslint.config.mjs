export default [
  {
    files: ["gnome-shell-extension/**/*.js"],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: "module",
      globals: {
        globalThis: "readonly",
        imports: "readonly",
        global: "readonly",
        log: "readonly",
        logError: "readonly",
      },
    },
    rules: {
      "no-unused-vars": ["warn", { argsIgnorePattern: "^_" }],
      "no-undef": "error",
      "prefer-const": "warn",
      eqeqeq: ["warn", "smart"],
    },
  },
];
