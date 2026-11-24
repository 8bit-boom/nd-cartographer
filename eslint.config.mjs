import globals from 'globals';

// The UI is one classic browser script (no modules, no bundler). These rules
// are correctness gates, not style: a syntax error, an undefined reference or a
// duplicate key fails the build. (The shipped v0.1.0 app.js had literal-newline
// string syntax errors that broke the whole UI — ESLint catches those instantly.)
export default [
  {
    files: ['ui/**/*.js'],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: 'script',
      globals: {
        ...globals.browser,
      },
    },
    rules: {
      'no-undef': 'error',
      'no-dupe-keys': 'error',
      'no-dupe-args': 'error',
      'no-unreachable': 'error',
      'no-constant-condition': 'error',
      'no-cond-assign': 'error',
      'no-sparse-arrays': 'error',
      'use-isnan': 'error',
      'valid-typeof': 'error',
      'no-unused-vars': ['warn', { args: 'none', varsIgnorePattern: '^_' }],
    },
  },
];