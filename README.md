## Still on beta! 
Please report any bugs you find in this repo.

# Draco injector 
A tool to inject into any Minecraft render dragon apk the ability to load shaders from global resourcepacks (It can't load shader resourcepacks from realms,servers and worlds, but if you add a shader resource pack on global resource packs list the shader you added works on all of those)

## Usage
1. Download binary for your platform from [releases](https://github.com/mcbegamerxx954/draco-injector/releases/latest).
2. Extract the archive.
<br><br>
![image](https://github.com/atashi764/draco-injector/assets/40156662/1b7eaf3f-4662-4ffa-98be-4981f49f8911)<br><br>
3. Run the binary in terminal.

<pre><u style="text-decoration-style:single"><b>Options:</b></u>
  <b>-a</b>, <b>--appname</b> &lt;APPNAME&gt;  New app name [default: &quot;Minecraft Patched(whar)&quot;]
  <b>-p</b>, <b>--pkgname</b> &lt;PKGNAME&gt;  New package name(optional)
  <b>-o</b>, <b>--output</b> &lt;OUTPUT&gt;    Output path
  <b>-h</b>, <b>--help</b>               Print help
  <b>-V</b>, <b>--version</b>            Print version</pre>


```injector <APK> -a <APPNAME> -p <PKGNAME> -o <OUTPUT>```

### Example command:
```
./injector Minecraft_1.20.73.01_arm-v8a_noMusic_972007501.apk -a HelloWorld -p com.moyang.minecraftpe -o HelloWorld.apk

```
During the process you will be asked whether to keep vanilla music or not. You can confirm your choice either by typing Y or N and enter.

This command selects the file named ```Minecraft_1.20.73.01_arm-v8a_noMusic_972007501.apk``` as input. Sets it's name to HelloWorld (shown in app list) and package id to ```com.moyang.minecraftpe``` and stores the output in the same directory as injector named ```HelloWorld.apk```.<br><br>
![image](https://github.com/atashi764/draco-injector/assets/40156662/e8919c22-d50f-4260-9871-cdc102a3dc22)
