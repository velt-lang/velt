import { defineApp } from "sigx";
import { ssrClientPlugin } from "@sigx/server-renderer/client";
import { App } from "./shared/App";

defineApp(<App path={window.location.pathname} />).use(ssrClientPlugin).hydrate!("#app");
