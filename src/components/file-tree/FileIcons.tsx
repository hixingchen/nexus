/* === Zed file icons — direct from zed assets, fill=currentColor === */
import folderSvg from '../../assets/folder.svg?raw';
import folderOpenSvg from '../../assets/folder_open.svg?raw';
import fileSvg from '../../assets/file_icons/file.svg?raw';
import typescriptSvg from '../../assets/file_icons/typescript.svg?raw';
import javascriptSvg from '../../assets/file_icons/javascript.svg?raw';
import pythonSvg from '../../assets/file_icons/python.svg?raw';
import rustSvg from '../../assets/file_icons/rust.svg?raw';
import goSvg from '../../assets/file_icons/go.svg?raw';
import htmlSvg from '../../assets/file_icons/html.svg?raw';
import cssSvg from '../../assets/file_icons/css.svg?raw';
import yamlSvg from '../../assets/file_icons/yaml.svg?raw';
import tomlSvg from '../../assets/file_icons/toml.svg?raw';
import javaSvg from '../../assets/file_icons/java.svg?raw';
import dockerSvg from '../../assets/file_icons/docker.svg?raw';
import vueSvg from '../../assets/file_icons/vue.svg?raw';
import sassSvg from '../../assets/file_icons/sass.svg?raw';
import terminalSvg from '../../assets/file_icons/terminal.svg?raw';
import imageSvg from '../../assets/file_icons/image.svg?raw';
import settingsSvg from '../../assets/file_icons/settings.svg?raw';
import gitSvg from '../../assets/file_icons/git.svg?raw';
import databaseSvg from '../../assets/file_icons/database.svg?raw';
import lockSvg from '../../assets/file_icons/lock.svg?raw';
import reactSvg from '../../assets/file_icons/react.svg?raw';
import codeSvg from '../../assets/file_icons/code.svg?raw';

const registry: Record<string, string> = {
  ts: typescriptSvg, tsx: typescriptSvg, mts: typescriptSvg, cts: typescriptSvg,
  js: javascriptSvg, jsx: reactSvg, mjs: javascriptSvg, cjs: javascriptSvg,
  py: pythonSvg, pyi: pythonSvg,
  rs: rustSvg,
  go: goSvg,
  html: htmlSvg, htm: htmlSvg,
  css: cssSvg, scss: sassSvg, sass: sassSvg, less: codeSvg,
  json: codeSvg, sql: databaseSvg, md: fileSvg, xml: codeSvg,
  yaml: yamlSvg, yml: yamlSvg,
  toml: tomlSvg,
  java: javaSvg,
  dockerfile: dockerSvg,
  lock: lockSvg,
  vue: vueSvg,
  svg: imageSvg, png: imageSvg, jpg: imageSvg, jpeg: imageSvg, gif: imageSvg, ico: imageSvg, webp: imageSvg,
  gitignore: gitSvg, gitattributes: gitSvg,
  sh: terminalSvg, bash: terminalSvg, zsh: terminalSvg, fish: terminalSvg, ps1: terminalSvg, bat: terminalSvg,
  env: settingsSvg, ini: settingsSvg, cfg: settingsSvg, conf: settingsSvg, editorconfig: settingsSvg,
};

export { folderSvg as FolderClosed, folderOpenSvg as FolderOpen };

export function getIconSvg(ext: string): string {
  return registry[ext.toLowerCase()] ?? fileSvg;
}
