// Vendored from Saltcorn 1: packages/saltcorn-builder/src/hooks/useTranslation.js
// at @saltcorn/builder 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/builder/vendor/README.md.
import { useContext } from "react";
import optionsCtx from "../components/context";

const useTranslation = () => {
  const options = useContext(optionsCtx);
  const translations = options.translations || {};
  const t = (phrase) => translations[phrase] || phrase;
  return { t };
};

export default useTranslation;