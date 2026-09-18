// ESLint 扁平配置（CQ-23 的 ②）。
//
// 为什么只接 lint、不接 prettier：格式化会产生一个覆盖几乎每个文件的 diff，把真正的
// 修复淹没在里面；而 lint 抓的是**语义**问题（hooks 依赖、无效断言、未处理 promise），
// 与格式无关。格式化留给将来单独一轮。
//
// 规则集的选择口径：只开"能抓到真实缺陷"的规则，不开风格规则。噪声规则（缩进/引号）
// 由 tailwind 与既有写法自然约束，写进 lint 只会训练大家忽略警告。
import js from '@eslint/js';
import tseslint from 'typescript-eslint';
import reactHooks from 'eslint-plugin-react-hooks';
import globals from 'globals';

export default tseslint.config(
  { ignores: ['dist/**', 'node_modules/**', 'src-tauri/**', '*.config.js'] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ['src/**/*.{ts,tsx}'],
    languageOptions: {
      globals: { ...globals.browser },
      parserOptions: { ecmaFeatures: { jsx: true } },
    },
    plugins: { 'react-hooks': reactHooks },
    rules: {
      // hooks 依赖数组：本仓库有过"守卫顺序反了/依赖漏了"的真实缺陷（PERF-6/PERF-9），
      // 但 exhaustive-deps 对"故意不写依赖"的既有写法会误报，故先设为 warn。
      'react-hooks/rules-of-hooks': 'error',
      'react-hooks/exhaustive-deps': 'warn',
      // 未使用变量：允许 `_` 前缀（既有代码用它标记"故意不用的形参"）
      '@typescript-eslint/no-unused-vars': ['error', {
        argsIgnorePattern: '^_',
        varsIgnorePattern: '^_',
        caughtErrors: 'all',
        caughtErrorsIgnorePattern: '^_',
      }],
      // 空的 catch 块正是本仓库反复收口的缺陷类型（CQ-16），必须拦住
      'no-empty': ['error', { allowEmptyCatch: false }],
    },
  },
  {
    // 测试文件：node 环境 + node:test 的 `t` 参数等
    files: ['src/**/__tests__/*.test.ts'],
    languageOptions: { globals: { ...globals.node } },
  },
);
