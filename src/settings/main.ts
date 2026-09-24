import { mount } from "svelte";
import "../lib/theme.css";
import { initTheme } from "../lib/theme";
import Settings from "./Settings.svelte";

initTheme();

const target = document.getElementById("app");
if (target) {
  mount(Settings, { target });
}
