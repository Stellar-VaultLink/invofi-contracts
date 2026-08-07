// Commitlint configuration for the CI Conventional Commits gate.
//
// We extend @commitlint/config-conventional and relax only the two rules
// that generate noise for documentation commits: body and footer lines are
// allowed to exceed 100 characters (long URLs, bullet summaries, and
// sentence-length prose are common in bodies and are not worth blocking CI).
//
// Everything else stays strict: header format/type-enum, subject case,
// empty-commit checks, etc. A PR with a non-conventional message still fails.
module.exports = {
  extends: ['@commitlint/config-conventional'],
  rules: {
    'body-max-line-length': [0], // body lines may exceed 100 chars
    'footer-max-line-length': [0], // footer lines (BREAKING CHANGE, refs) too
  },
};
