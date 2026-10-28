export default {
  extends: ["@commitlint/config-conventional"],
  rules: {
    "body-max-line-length": [0, "never", 9999],
    "footer-max-line-length": [0, "never", 9999],
  },
};
