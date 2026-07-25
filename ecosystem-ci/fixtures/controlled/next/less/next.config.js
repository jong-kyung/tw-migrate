const withLess = require('next-with-less');

module.exports = withLess({
  webpack(config) {
    const cssRule = config.module.rules.find((rule) => rule.oneOf?.some((child) => child.test?.source === '\\.module\\.less$'));
    for (const rule of cssRule.oneOf) {
      if (rule[Symbol.for('__next_css_remove')] && rule.test instanceof RegExp) {
        rule.test = new RegExp(rule.test.source.replace('|less', ''), rule.test.flags);
      }
    }
    return config;
  },
});
