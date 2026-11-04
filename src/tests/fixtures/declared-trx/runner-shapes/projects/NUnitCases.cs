using NUnit.Framework;

namespace DeclaredNUnitCapture;

[TestFixture]
[NonParallelizable]
public class CaptureCases
{
#if CAPTURE_PASS
    [Test]
    public void Passing() => Assert.That(2 + 2, Is.EqualTo(4));
#elif CAPTURE_FAIL
    [Test]
    public void Failing() => Assert.Fail("Intentional captured test failure.");
#endif
}
