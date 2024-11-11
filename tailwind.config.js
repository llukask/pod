/** @type {import('tailwindcss').Config} */
module.exports = {
  content: ["./templates/**/*.html"],
  theme: {
    extend: {
      fontFamily: {
        serif: ["ETBembo", "ui-serif"],
        sans: ["Inter", "ui-sans-serif"],
        mono: ["MonoLisa", "ui-monospace"],
      },
    },
  },
  plugins: [],
};
