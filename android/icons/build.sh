#!/usr/bin/env bash
# LangeIcons.java -> build/lange-icons.jar (dex). Compila contra stubs mínimos da API Android
# (stubs.java.txt: uma classe por linha, só p/ o javac; o Android real fornece as classes) e
# converte com o d8 (r8 do Google Maven, baixado uma vez).
set -euo pipefail
cd "$(dirname "$0")"
V=8.5.35
R8=build/r8-$V.jar
OUT=build/lange-icons.jar
mkdir -p build
if [ ! -f $R8 ]; then
  curl -sfL -o $R8.tmp https://dl.google.com/android/maven2/com/android/tools/r8/$V/r8-$V.jar
  mv $R8.tmp $R8
fi
[ $OUT -nt LangeIcons.java ] && exit 0
rm -rf build/src build/stubcls build/cls && mkdir -p build/src build/stubcls build/cls
while read -r line; do
  pkg=$(sed -E 's/^package ([^;]+);.*/\1/' <<<"$line"); cls=$(sed -E 's/^package [^;]+; public (abstract )?class ([A-Za-z]+).*/\2/' <<<"$line")
  mkdir -p "build/src/${pkg//.//}"; echo "$line" > "build/src/${pkg//.//}/$cls.java"
done < stubs.java.txt
javac --release 8 -nowarn -d build/stubcls $(find build/src -name '*.java')
javac --release 8 -nowarn -cp build/stubcls -d build/cls LangeIcons.java
java -cp $R8 com.android.tools.r8.D8 --min-api 26 --output $OUT build/cls/LangeIcons.class
