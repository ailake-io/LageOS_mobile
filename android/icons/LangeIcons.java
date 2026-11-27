// Ícones dos apps renderizados pelo próprio Android (vetor, adaptativo, PNG: tudo vira bitmap).
// Roda no container via app_process. Uso: LangeIcons TAM comp1 comp2 ...
// Saída: uma linha por app, "componente TAM hex(ARGB)".
import android.content.ComponentName;
import android.content.Context;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.drawable.Drawable;
import java.io.PrintStream;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;

public class LangeIcons {
    public static void main(String[] args) {
        try { run(args); } catch (Throwable e) { System.out.println("! " + e); }
    }

    static void run(String[] args) throws Exception {
        android.os.Looper.prepareMainLooper();
        // sem app: contexto de sistema via ActivityThread (mesmo truque do scrcpy)
        Class<?> at = Class.forName("android.app.ActivityThread");
        Constructor<?> ctor = at.getDeclaredConstructor();
        ctor.setAccessible(true);
        Object thread = ctor.newInstance();
        Field cur = at.getDeclaredField("sCurrentActivityThread");
        cur.setAccessible(true);
        cur.set(null, thread);
        Context ctx = (Context) at.getDeclaredMethod("getSystemContext").invoke(thread);
        // AdaptiveIconDrawable lê a máscara via ActivityThread.currentApplication().getResources():
        // sem app, NPE. Registra um Application ligado ao contexto de sistema.
        Class<?> appCls = Class.forName("android.app.Application");
        Object app = appCls.getDeclaredConstructor().newInstance();
        java.lang.reflect.Method attach = appCls.getDeclaredMethod("attach", Context.class);
        attach.setAccessible(true);
        attach.invoke(app, ctx);
        Field initial = at.getDeclaredField("mInitialApplication");
        initial.setAccessible(true);
        initial.set(thread, app);
        PackageManager pm = ctx.getPackageManager();
        int n = Integer.parseInt(args[0]);
        PrintStream out = new PrintStream(new java.io.BufferedOutputStream(System.out, 1 << 16));
        int[] px = new int[n * n];
        StringBuilder sb = new StringBuilder(n * n * 8 + 128);
        for (int i = 1; i < args.length; i++) {
            try {
                // recursos do próprio app com densidade explícita (o contexto de sistema do app_process
                // não tem configuração de tela; getActivityIcon caía no ícone padrão e falhava)
                android.content.pm.ActivityInfo ai = pm.getActivityInfo(ComponentName.unflattenFromString(args[i]), 0);
                // tema do app: ícones vetoriais usam ?attr (cores do tema)
                android.content.res.Resources res = pm.getResourcesForApplication(ai.applicationInfo);
                android.content.res.Resources.Theme theme = res.newTheme();
                if (ai.getThemeResource() != 0) theme.applyStyle(ai.getThemeResource(), true);
                Drawable d = res.getDrawableForDensity(ai.getIconResource(), 320, theme);
                Bitmap b = Bitmap.createBitmap(n, n, Bitmap.Config.ARGB_8888);
                d.setBounds(0, 0, n, n);
                d.draw(new Canvas(b));
                b.getPixels(px, 0, n, 0, 0, n, n);
                sb.setLength(0);
                sb.append(args[i]).append(' ').append(n).append(' ');
                for (int p : px) {
                    String h = Integer.toHexString(p);
                    for (int k = h.length(); k < 8; k++) sb.append('0');
                    sb.append(h);
                }
                out.println(sb);
            } catch (Exception e) {
                // app sem ícone ou removido: o shell mantém o ícone de letra; motivo p/ o log do langedroid
                StringBuilder why = new StringBuilder("! " + args[i]);
                for (Throwable c = e; c != null; c = c.getCause()) why.append(" <- ").append(c);
                out.println(why);
            }
        }
        out.flush();
    }
}
